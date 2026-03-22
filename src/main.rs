use std::io::{self, BufRead, BufReader, BufWriter, Write};
use std::os::unix::net::UnixStream;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::Parser;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use rand::seq::SliceRandom;
use serde::Deserialize;

#[derive(Parser, Debug)]
#[command(name = "muz")]
#[command(version)]
#[command(about = "Randomly loops through a YouTube playlist and plays audio-only")]
struct Args {
    #[arg(help = "YouTube playlist URL")]
    playlist_url: String,

    #[arg(
        long,
        default_value = "5",
        help = "Delay in seconds before retrying after playlist-fetch failures"
    )]
    retry_delay_secs: u64,
}

#[derive(Debug, Clone)]
struct Track {
    id: String,
    title: String,
}

struct MpvIpc {
    writer: BufWriter<UnixStream>,
    reader: BufReader<UnixStream>,
}

impl MpvIpc {
    fn connect_with_retry(path: &str, timeout: Duration) -> Result<Self> {
        let deadline = Instant::now() + timeout;
        loop {
            match UnixStream::connect(path) {
                Ok(stream) => {
                    stream
                        .set_read_timeout(Some(Duration::from_millis(10)))
                        .context("Failed to set read timeout on mpv IPC socket")?;
                    let reader_stream = stream
                        .try_clone()
                        .context("Failed to clone mpv IPC socket for reading")?;
                    return Ok(Self {
                        writer: BufWriter::new(stream),
                        reader: BufReader::new(reader_stream),
                    });
                }
                Err(_) if Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(50));
                }
                Err(e) => {
                    bail!("Timed out waiting for mpv IPC socket at {path}: {e}");
                }
            }
        }
    }

    fn set_pause(&mut self, paused: bool) -> Result<()> {
        let cmd = format!(
            "{{\"command\": [\"set_property\", \"pause\", {}]}}\n",
            paused
        );
        self.writer
            .write_all(cmd.as_bytes())
            .context("Failed to write pause command to mpv IPC")?;
        self.writer
            .flush()
            .context("Failed to flush pause command to mpv IPC")?;
        self.drain_events();
        Ok(())
    }

    fn drain_events(&mut self) {
        let mut line = String::new();
        loop {
            line.clear();
            match self.reader.read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
        }
    }
}

#[derive(Deserialize)]
struct PlaylistResponse {
    entries: Vec<PlaylistEntry>,
}

#[derive(Deserialize)]
struct PlaylistEntry {
    id: Option<String>,
    title: Option<String>,
}

#[derive(Debug)]
enum UserCommand {
    Next,
    PauseToggle,
    Quit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PlaybackResult {
    Finished,
    Skipped,
    QuitRequested,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let raw_mode_guard = RawModeGuard::enable()?;
    let command_rx = spawn_command_listener();

    ensure_command_available("yt-dlp", &["--version"])?;
    ensure_command_available("mpv", &["--version"])?;

    print_info_line(&format!(
        "Starting random audio playback loop for: {}",
        args.playlist_url
    ))?;
    print_info_line("Commands: n=skip, p=pause/resume, q=quit, Ctrl+C=stop.")?;
    print_info_line("")?;

    let retry_delay = Duration::from_secs(args.retry_delay_secs);
    let mut cycle = 1usize;

    loop {
        match fetch_playlist_tracks(&args.playlist_url) {
            Ok(mut tracks) => {
                if tracks.is_empty() {
                    print_error_line(&format!("[cycle {cycle}] Playlist had no playable items."))?;
                    thread::sleep(retry_delay);
                    continue;
                }

                tracks.shuffle(&mut rand::rng());
                print_info_line(&format!(
                    "[cycle {cycle}] Playing {} tracks in random order",
                    tracks.len()
                ))?;

                for (index, track) in tracks.iter().enumerate() {
                    drain_pending_commands(&command_rx);

                    let ordinal = index + 1;
                    clear_status_line()?;
                    print_info_line("")?;
                    print_info_line(&format!("[{ordinal}/{}] {}", tracks.len(), track.title))?;

                    match play_track_audio(track, &command_rx) {
                        Ok(PlaybackResult::Finished) => {}
                        Ok(PlaybackResult::Skipped) => {
                            clear_status_line()?;
                            print_info_line("Skipped.")?;
                        }
                        Ok(PlaybackResult::QuitRequested) => {
                            clear_status_line()?;
                            drop(raw_mode_guard);
                            print_info_line("Quit requested. Exiting.")?;
                            return Ok(());
                        }
                        Err(error) => {
                            print_error_line(&format!(
                                "Failed to play '{}': {error:#}",
                                track.title
                            ))?;
                        }
                    }
                }

                cycle += 1;
            }
            Err(error) => {
                print_error_line(&format!(
                    "[cycle {cycle}] Failed to fetch playlist: {error:#}"
                ))?;
                print_error_line(&format!(
                    "Retrying in {} second(s)...",
                    args.retry_delay_secs
                ))?;
                thread::sleep(retry_delay);
            }
        }
    }
}

struct RawModeGuard;

impl RawModeGuard {
    fn enable() -> Result<Self> {
        enable_raw_mode().context("Failed to enable terminal raw mode")?;
        Ok(Self)
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
    }
}

fn spawn_command_listener() -> mpsc::Receiver<UserCommand> {
    let (tx, rx) = mpsc::channel::<UserCommand>();

    thread::spawn(move || {
        while let Ok(has_event) = event::poll(Duration::from_millis(200)) {
            if !has_event {
                continue;
            }

            match event::read() {
                Ok(Event::Key(key_event)) if key_event.kind == KeyEventKind::Press => {
                    let parsed = match key_event.code {
                        KeyCode::Char('n') | KeyCode::Char('N') => Some(UserCommand::Next),
                        KeyCode::Char('p') | KeyCode::Char('P') => Some(UserCommand::PauseToggle),
                        KeyCode::Char('q') | KeyCode::Char('Q') => Some(UserCommand::Quit),
                        _ => None,
                    };

                    if let Some(command) = parsed
                        && tx.send(command).is_err()
                    {
                        break;
                    }
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
    });

    rx
}

fn drain_pending_commands(command_rx: &mpsc::Receiver<UserCommand>) {
    while command_rx.try_recv().is_ok() {}
}

fn ensure_command_available(command: &str, check_args: &[&str]) -> Result<()> {
    let status = Command::new(command)
        .args(check_args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .with_context(|| format!("Could not execute '{command}'. Is it installed?"))?;

    if status.success() {
        Ok(())
    } else {
        bail!("'{command}' is installed but did not run successfully")
    }
}

fn set_status_line(status: &str) -> Result<()> {
    print!("\r{status}\x1b[K");
    io::stdout()
        .flush()
        .context("Failed to flush status line to terminal")?;
    Ok(())
}

fn set_playback_status_line(
    status: &str,
    elapsed: Duration,
    total_duration: Option<Duration>,
) -> Result<()> {
    let elapsed_text = format_duration(elapsed);
    let total_text = total_duration
        .map(format_duration)
        .unwrap_or_else(|| "--:--".to_string());

    set_status_line(&format!("Status: [{status}] {elapsed_text} / {total_text}"))
}

fn format_duration(duration: Duration) -> String {
    let total_secs = duration.as_secs();
    let minutes = total_secs / 60;
    let seconds = total_secs % 60;
    format!("{minutes:02}:{seconds:02}")
}

fn clear_status_line() -> Result<()> {
    print!("\r\x1b[K");
    io::stdout()
        .flush()
        .context("Failed to clear status line in terminal")?;
    Ok(())
}

fn print_info_line(message: &str) -> Result<()> {
    print!("\r{message}\r\n");
    io::stdout()
        .flush()
        .context("Failed to write info output to terminal")?;
    Ok(())
}

fn print_error_line(message: &str) -> Result<()> {
    eprint!("\r{message}\r\n");
    io::stderr()
        .flush()
        .context("Failed to write error output to terminal")?;
    Ok(())
}

fn fetch_playlist_tracks(playlist_url: &str) -> Result<Vec<Track>> {
    let output = Command::new("yt-dlp")
        .args(["--flat-playlist", "--dump-single-json", playlist_url])
        .output()
        .context("Failed to run yt-dlp for playlist extraction")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("yt-dlp playlist extraction failed: {stderr}");
    }

    let payload = String::from_utf8(output.stdout)
        .context("yt-dlp output was not valid UTF-8 while reading playlist JSON")?;

    let parsed: PlaylistResponse =
        serde_json::from_str(&payload).context("Failed to parse playlist JSON")?;

    let tracks = parsed
        .entries
        .into_iter()
        .filter_map(|entry| match entry.id {
            Some(id) => Some(Track {
                id,
                title: entry
                    .title
                    .unwrap_or_else(|| "(untitled track)".to_string()),
            }),
            None => None,
        })
        .collect();

    Ok(tracks)
}

fn play_track_audio(
    track: &Track,
    command_rx: &mpsc::Receiver<UserCommand>,
) -> Result<PlaybackResult> {
    let video_url = format!("https://www.youtube.com/watch?v={}", track.id);
    let socket_path = format!("/tmp/muz-mpv-{}.sock", std::process::id());

    let total_duration = fetch_track_duration(&video_url);

    let mut child = Command::new("mpv")
        .args([
            "--no-video",
            "--ytdl",
            "--really-quiet",
            &format!("--input-ipc-server={socket_path}"),
            &video_url,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("Failed to launch mpv")?;

    let mut ipc = match MpvIpc::connect_with_retry(&socket_path, Duration::from_secs(15)) {
        Ok(ipc) => ipc,
        Err(e) => {
            let _ = child.kill();
            let _ = child.wait();
            let _ = std::fs::remove_file(&socket_path);
            return Err(e);
        }
    };

    let playback_started_at = Instant::now();
    let mut is_paused = false;
    let mut paused_started_at: Option<Instant> = None;
    let mut paused_total = Duration::ZERO;

    set_playback_status_line("playing", Duration::ZERO, total_duration)?;

    let result = loop {
        let now = Instant::now();
        let elapsed = if let Some(paused_at) = paused_started_at {
            paused_at.duration_since(playback_started_at) - paused_total
        } else {
            now.duration_since(playback_started_at) - paused_total
        };

        set_playback_status_line(
            if is_paused { "paused" } else { "playing" },
            elapsed,
            total_duration,
        )?;

        ipc.drain_events();

        if let Some(status) = child.try_wait().context("Failed while waiting for mpv")? {
            clear_status_line()?;
            if status.success() {
                break PlaybackResult::Finished;
            }
            bail!("mpv exited with a non-zero status: {:?}", status.code());
        }

        match command_rx.try_recv() {
            Ok(UserCommand::Next) => {
                child.kill().context("Failed to stop mpv for skip")?;
                child.wait().context("Failed to wait for mpv after skip")?;
                clear_status_line()?;
                break PlaybackResult::Skipped;
            }
            Ok(UserCommand::PauseToggle) => {
                if is_paused {
                    ipc.set_pause(false)?;
                    if let Some(paused_at) = paused_started_at.take() {
                        paused_total += Instant::now().duration_since(paused_at);
                    }
                    is_paused = false;
                } else {
                    ipc.set_pause(true)?;
                    paused_started_at = Some(Instant::now());
                    is_paused = true;
                }
            }
            Ok(UserCommand::Quit) => {
                child.kill().context("Failed to stop mpv for quit")?;
                child.wait().context("Failed to wait for mpv after quit")?;
                clear_status_line()?;
                break PlaybackResult::QuitRequested;
            }
            Err(mpsc::TryRecvError::Empty | mpsc::TryRecvError::Disconnected) => {
                thread::sleep(Duration::from_millis(200));
            }
        }
    };

    let _ = std::fs::remove_file(&socket_path);
    Ok(result)
}

fn fetch_track_duration(video_url: &str) -> Option<Duration> {
    let output = Command::new("yt-dlp")
        .args(["--no-playlist", "--print", "%(duration)s", video_url])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    String::from_utf8(output.stdout)
        .ok()?
        .lines()
        .next()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .and_then(|l| l.parse::<u64>().ok())
        .map(Duration::from_secs)
}
