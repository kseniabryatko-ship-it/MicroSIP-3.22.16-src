use microsip_core::audio::{Microphone, Speaker};
use std::error::Error;
use std::thread;
use std::time::{Duration, Instant};

fn main() -> Result<(), Box<dyn Error>> {
    let duration = parse_duration()?;

    println!("Starting microphone...");

    let microphone = Microphone::start()?;

    println!("Starting speaker...");

    let speaker = Speaker::start()?;

    println!(
        "Microphone format: {} Hz, {} channel(s)",
        microphone.format().sample_rate,
        microphone.format().channels
    );

    println!(
        "Speaker format: {} Hz, {} channel(s)",
        speaker.format().sample_rate,
        speaker.format().channels
    );

    println!(
        "Recording and playing locally for {} seconds...",
        duration.as_secs()
    );

    let started = Instant::now();

    while started.elapsed() < duration {
        if let Some(samples) = microphone.read_chunk()? {
            speaker.play_mono(&samples, microphone.format().sample_rate)?;
        }

        thread::sleep(Duration::from_millis(5));
    }

    // Даём аудиоустройству дописать оставшийся буфер.
    thread::sleep(Duration::from_millis(500));

    println!("Local microphone-to-speaker test completed");

    Ok(())
}

fn parse_duration() -> Result<Duration, Box<dyn Error>> {
    let seconds = std::env::args()
        .nth(1)
        .map(|value| value.parse::<u64>())
        .transpose()?
        .unwrap_or(5);

    if seconds == 0 {
        return Err("Duration must be greater than zero".into());
    }

    Ok(Duration::from_secs(seconds))
}
