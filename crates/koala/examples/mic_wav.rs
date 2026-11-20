//! Decodes a dumped microphone stream to a WAV, for listening to a trial.
//!
//! ```text
//! cargo run --release --example mic_wav -- PAYLOADS OUT.wav
//! ```
//!
//! `PAYLOADS` holds each decrypted RTP payload after its length (u16,
//! big-endian), in order. `OUT.wav` gets all 4 channels, 16-bit at 48 kHz.
//! Prints each channel's level and how alike the channels are.

use koala_capture::audio::{CHANNELS, FRAME_SAMPLES, SAMPLE_RATE, channel_frames};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [input, output] = &args[..] else {
        eprintln!("usage: mic_wav PAYLOADS OUT.wav");
        std::process::exit(2);
    };
    let data = std::fs::read(input).expect("reading the payloads");
    let mut decoders: Vec<opus::Decoder> = (0..CHANNELS)
        .map(|_| opus::Decoder::new(SAMPLE_RATE, opus::Channels::Mono).expect("an Opus decoder"))
        .collect();
    let mut channels: Vec<Vec<i16>> = vec![Vec::new(); CHANNELS];
    let (mut packets, mut bad, mut errors) = (0usize, 0usize, 0usize);
    let mut at = 0;
    while at + 2 <= data.len() {
        let len = usize::from(u16::from_be_bytes([data[at], data[at + 1]]));
        let payload = &data[at + 2..at + 2 + len];
        at += 2 + len;
        packets += 1;
        let Ok(frames) = channel_frames(payload).map_err(|e| eprintln!("packet {packets}: {e}")) else {
            bad += 1;
            continue;
        };
        if frames.len() != CHANNELS {
            bad += 1;
            continue;
        }
        for (c, frame) in frames.iter().enumerate() {
            let mut pcm = [0i16; FRAME_SAMPLES * 2];
            match decoders[c].decode(frame, &mut pcm, false) {
                Ok(n) => channels[c].extend_from_slice(&pcm[..n]),
                Err(e) => {
                    errors += 1;
                    if errors <= 3 {
                        eprintln!("packet {packets}, channel {c}: {e}");
                    }
                    channels[c].extend_from_slice(&[0; FRAME_SAMPLES]);
                }
            }
        }
    }
    let samples = channels.iter().map(Vec::len).min().unwrap_or(0);
    println!(
        "{packets} packets, {bad} that didn't split into {CHANNELS} channels, {errors} decode errors; {:.1} s",
        samples as f64 / f64::from(SAMPLE_RATE)
    );
    let rms = |c: &[i16]| (c.iter().map(|&s| f64::from(s).powi(2)).sum::<f64>() / c.len().max(1) as f64).sqrt();
    for (i, c) in channels.iter().enumerate() {
        let peak = c.iter().map(|s| s.unsigned_abs()).max().unwrap_or(0);
        println!("channel {i}: RMS {:.0}, peak {peak}", rms(&c[..samples]));
    }
    for i in 1..CHANNELS {
        let (a, b) = (&channels[0][..samples], &channels[i][..samples]);
        let dot: f64 = a.iter().zip(b).map(|(&x, &y)| f64::from(x) * f64::from(y)).sum();
        let norm = (rms(a) * rms(b) * samples as f64).max(1.0);
        println!("correlation of channels 0 and {i}: {:.2}", dot / norm);
    }
    write_wav(output, &channels, samples).expect("writing the WAV");
    println!("wrote {output}");
}

/// A 16-bit PCM WAV, the channels interleaved.
fn write_wav(path: &str, channels: &[Vec<i16>], samples: usize) -> std::io::Result<()> {
    let n = channels.len() as u16;
    let data_len = (samples * channels.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&n.to_le_bytes());
    out.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    out.extend_from_slice(&(SAMPLE_RATE * u32::from(n) * 2).to_le_bytes());
    out.extend_from_slice(&(n * 2).to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for s in 0..samples {
        for c in channels {
            out.extend_from_slice(&c[s].to_le_bytes());
        }
    }
    std::fs::write(path, out)
}
