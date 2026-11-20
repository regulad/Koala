//! The microphone's RTP payload: AVConference codec type 19, which Koala's
//! AVC answer gets as payload type 101.
//!
//! One Opus frame per channel, the channels in order, each after its length
//! (u16, big-endian). In 'call' mode there are 4 channels, the iPhone's
//! separate microphones (Parker, listening to trial 22), at 48 kHz, one
//! 10 ms frame each (RTP timestamps advance 480 a packet). Every frame
//! seen is CELT-only fullband 10 ms mono (TOC byte `0xf0`); a stream opens
//! with Opus's 3-byte silence frame (`f0 ff fe`) on each channel. From
//! trial 22's decrypted stream (2026-11-19): 11,745 packets, 1,050–1,060
//! bytes each once speaking.

/// The channels in 'call' mode.
pub const CHANNELS: usize = 4;
/// The sample rate.
pub const SAMPLE_RATE: u32 = 48_000;
/// Samples per channel in one packet: 10 ms.
pub const FRAME_SAMPLES: usize = 480;

/// Why a payload doesn't split.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum AudioError {
    #[error("a channel's length runs past the payload")]
    Truncated,
    #[error("bytes after the last channel")]
    Trailing,
}

/// A payload's frames, one per channel, in order.
pub fn channel_frames(payload: &[u8]) -> Result<Vec<&[u8]>, AudioError> {
    let mut frames = Vec::with_capacity(CHANNELS);
    let mut rest = payload;
    while !rest.is_empty() {
        let (len, after) = match rest {
            [hi, lo, after @ ..] => (usize::from(u16::from_be_bytes([*hi, *lo])), after),
            _ => return Err(AudioError::Trailing),
        };
        if after.len() < len {
            return Err(AudioError::Truncated);
        }
        let (frame, next) = after.split_at(len);
        frames.push(frame);
        rest = next;
    }
    Ok(frames)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trial 22's first packet: Opus silence on all 4 channels.
    #[test]
    fn the_opening_silence_splits_into_four_channels() {
        let payload = [0, 3, 0xf0, 0xff, 0xfe, 0, 3, 0xf0, 0xff, 0xfe, 0, 3, 0xf0, 0xff, 0xfe, 0, 3, 0xf0, 0xff, 0xfe];
        let frames = channel_frames(&payload).unwrap();
        assert_eq!(frames, vec![&[0xf0, 0xff, 0xfe][..]; CHANNELS]);
    }

    #[test]
    fn a_cut_or_padded_payload_is_refused() {
        assert_eq!(channel_frames(&[0, 5, 0xf0, 1]), Err(AudioError::Truncated));
        assert_eq!(channel_frames(&[0, 1, 0xf0, 7]), Err(AudioError::Trailing));
        assert_eq!(channel_frames(&[]), Ok(vec![]));
    }
}
