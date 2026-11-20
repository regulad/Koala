//! RTCP for the microphone stream, as its receiver (RFC 3550): statistics
//! on the iPhone's RTP, and the receiver reports a host sends back.
//!
//! The iPhone ends the session if no RTCP arrives within 12 s
//! (`rtcpTimeoutInterval=12.0`, trials 17–21). Its own RTCP, about once a
//! second (trial 21), is a compound packet: a sender report with one report
//! block about the host's SSRC, and an SDES whose CNAME is empty. Koala,
//! which sends no media, answers the same way with a receiver report.

use std::time::Instant;

/// RTCP packet types.
pub mod kind {
    pub const SENDER_REPORT: u8 = 200;
    pub const RECEIVER_REPORT: u8 = 201;
    pub const SDES: u8 = 202;
}

/// A sequence number gap larger than this is a restart, not loss (RFC 3550
/// appendix A.1's MAX_DROPOUT).
const MAX_DROPOUT: u16 = 3000;

/// What a receiver keeps about one source (RFC 3550 appendix A.1, A.3, A.8).
#[derive(Clone, Debug)]
pub struct ReceiverStats {
    /// The source's SSRC.
    pub ssrc: u32,
    clock_rate: u32,
    base_seq: u16,
    max_seq: u16,
    cycles: u32,
    received: u32,
    expected_prior: u32,
    received_prior: u32,
    /// The previous packet's relative transit time, in timestamp units.
    transit: Option<i64>,
    /// Interarrival jitter, in timestamp units, ×16 (A.8's integer form).
    jitter: u32,
    /// The middle 32 bits of the latest sender report's NTP time, and when
    /// it arrived.
    last_sr: Option<(u32, Instant)>,
    /// When the statistics start, for arrival times in timestamp units.
    epoch: Instant,
}

impl ReceiverStats {
    /// Statistics on `ssrc`'s packets, its RTP clock at `clock_rate` Hz, from
    /// its first packet's sequence number.
    pub fn new(ssrc: u32, clock_rate: u32, first_seq: u16) -> Self {
        Self {
            ssrc,
            clock_rate,
            base_seq: first_seq,
            max_seq: first_seq.wrapping_sub(1),
            cycles: 0,
            received: 0,
            expected_prior: 0,
            received_prior: 0,
            transit: None,
            jitter: 0,
            last_sr: None,
            epoch: Instant::now(),
        }
    }

    /// Takes in one RTP packet, received at `arrival`.
    pub fn received(&mut self, seq: u16, timestamp: u32, arrival: Instant) {
        let delta = seq.wrapping_sub(self.max_seq);
        if delta < MAX_DROPOUT {
            if seq < self.max_seq {
                self.cycles += 1 << 16;
            }
            self.max_seq = seq;
        } else if delta <= u16::MAX - 100 {
            // A large jump: the source restarted. Start over from it.
            *self = Self { last_sr: self.last_sr, epoch: self.epoch, ..Self::new(self.ssrc, self.clock_rate, seq) };
            self.max_seq = seq;
        }
        // Otherwise a duplicate or a late packet: counted, not the maximum.
        self.received += 1;
        let arrival_units = arrival.duration_since(self.epoch).as_secs_f64() * f64::from(self.clock_rate);
        let transit = arrival_units as i64 - i64::from(timestamp);
        if let Some(previous) = self.transit {
            let d = (transit - previous).unsigned_abs() as u32;
            // J += (|D| - J) / 16, kept ×16.
            self.jitter = self.jitter.wrapping_add(d).wrapping_sub((self.jitter + 8) >> 4);
        }
        self.transit = Some(transit);
    }

    /// Takes in a sender report from the source: its NTP time's middle 32
    /// bits, received at `arrival`.
    pub fn sender_report(&mut self, ntp_middle: u32, arrival: Instant) {
        self.last_sr = Some((ntp_middle, arrival));
    }

    /// The highest sequence number received, extended by its wraps.
    pub fn extended_max(&self) -> u32 {
        self.cycles + u32::from(self.max_seq)
    }

    /// Packets received so far.
    pub fn packets(&self) -> u32 {
        self.received
    }

    /// A report block on the source, as of `now`; starts the next interval.
    pub fn report_block(&mut self, now: Instant) -> [u8; 24] {
        let expected = self.extended_max().wrapping_sub(u32::from(self.base_seq)).wrapping_add(1);
        let lost = i64::from(expected) - i64::from(self.received);
        let lost = lost.clamp(-0x80_0000, 0x7f_ffff) as i32;
        let expected_interval = expected.wrapping_sub(self.expected_prior);
        let received_interval = self.received.wrapping_sub(self.received_prior);
        self.expected_prior = expected;
        self.received_prior = self.received;
        let lost_interval = i64::from(expected_interval) - i64::from(received_interval);
        let fraction = if expected_interval == 0 || lost_interval <= 0 {
            0
        } else {
            ((lost_interval << 8) / i64::from(expected_interval)) as u8
        };
        let (lsr, dlsr) = match self.last_sr {
            // The delay since it, in units of 1/65536 s.
            Some((ntp, at)) => (ntp, (now.duration_since(at).as_secs_f64() * 65536.0) as u32),
            None => (0, 0),
        };
        let mut block = [0u8; 24];
        block[0..4].copy_from_slice(&self.ssrc.to_be_bytes());
        block[4] = fraction;
        block[5..8].copy_from_slice(&(lost as u32).to_be_bytes()[1..]);
        block[8..12].copy_from_slice(&self.extended_max().to_be_bytes());
        block[12..16].copy_from_slice(&(self.jitter >> 4).to_be_bytes());
        block[16..20].copy_from_slice(&lsr.to_be_bytes());
        block[20..24].copy_from_slice(&dlsr.to_be_bytes());
        block
    }
}

/// A compound RTCP packet from `ssrc`, who sends no media: a receiver report
/// with `blocks`, then an SDES whose CNAME is empty, as the iPhone's is.
pub fn receiver_report(ssrc: u32, blocks: &[[u8; 24]]) -> Vec<u8> {
    let count = blocks.len().min(31) as u8;
    let mut out = Vec::with_capacity(8 + 24 * usize::from(count) + 12);
    out.push(0x80 | count);
    out.push(kind::RECEIVER_REPORT);
    out.extend_from_slice(&(1 + 6 * u16::from(count)).to_be_bytes());
    out.extend_from_slice(&ssrc.to_be_bytes());
    for b in &blocks[..usize::from(count)] {
        out.extend_from_slice(b);
    }
    // SDES, one chunk: the SSRC, CNAME (1) of length 0, the end, padding.
    out.extend_from_slice(&[0x81, kind::SDES, 0x00, 0x02]);
    out.extend_from_slice(&ssrc.to_be_bytes());
    out.extend_from_slice(&[0x01, 0x00, 0x00, 0x00]);
    out
}

/// The sender reports in a compound RTCP packet: each sender's SSRC and the
/// middle 32 bits of its NTP time.
pub fn sender_reports(rtcp: &[u8]) -> Vec<(u32, u32)> {
    let mut out = Vec::new();
    let mut at = 0;
    while at + 4 <= rtcp.len() {
        let words = usize::from(u16::from_be_bytes([rtcp[at + 2], rtcp[at + 3]]));
        let end = at + 4 * (words + 1);
        if rtcp[at] >> 6 != 2 || end > rtcp.len() {
            break;
        }
        if rtcp[at + 1] == kind::SENDER_REPORT && end - at >= 28 {
            let p = &rtcp[at..end];
            let ssrc = u32::from_be_bytes([p[4], p[5], p[6], p[7]]);
            // NTP seconds in 8..12, fraction in 12..16: the middle 32 bits.
            let middle = u32::from_be_bytes([p[10], p[11], p[12], p[13]]);
            out.push((ssrc, middle));
        }
        at = end;
    }
    out
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn a_receiver_report_is_well_formed() {
        let block = [7u8; 24];
        let rr = receiver_report(0x1ed2_1a64, &[block]);
        assert_eq!(rr.len(), 32 + 12);
        assert_eq!(&rr[..8], &[0x81, 201, 0x00, 0x07, 0x1e, 0xd2, 0x1a, 0x64]);
        assert_eq!(&rr[8..32], &block);
        // The SDES, as the iPhone's: 81ca0002, the SSRC, 01000000.
        assert_eq!(&rr[32..], &[0x81, 202, 0, 2, 0x1e, 0xd2, 0x1a, 0x64, 1, 0, 0, 0]);
        // Every part's length field covers it exactly.
        assert_eq!(sender_reports(&rr), vec![]);
    }

    /// The iPhone's first sender report from trial 21 (decrypted), with its
    /// SDES.
    #[test]
    fn the_iphones_sender_report_is_read() {
        let sr = [
            0x81, 0xc8, 0x00, 0x0c, 0x97, 0x0b, 0x76, 0xab, 0xee, 0xa9, 0x99, 0xad, 0xb9, 0x40, 0x78, 0x00, 0x00, 0x00,
            0xa9, 0xe5, 0x00, 0x00, 0x00, 0x5b, 0x00, 0x01, 0x87, 0xbb, 0x1e, 0xd2, 0x1a, 0x64, 0x00, 0x00, 0x00, 0x01,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x81, 0xca, 0x00, 0x02, 0x97, 0x0b, 0x76, 0xab, 0x01, 0x00,
            0x00, 0x00,
        ];
        assert_eq!(sender_reports(&sr), vec![(0x970b_76ab, 0x99ad_b940)]);
    }

    #[test]
    fn statistics_count_loss_and_report_the_last_sender_report() {
        let start = Instant::now();
        let mut s = ReceiverStats::new(0x970b_76ab, 48_000, 100);
        // 10 packets every 10 ms (480 units), 3 of them lost.
        for i in 0..10u16 {
            if [3, 4, 7].contains(&i) {
                continue;
            }
            s.received(100 + i, 480 * u32::from(i), start + Duration::from_millis(10 * u64::from(i)));
        }
        s.sender_report(0x1234_5678, start + Duration::from_millis(50));
        let block = s.report_block(start + Duration::from_millis(100));
        assert_eq!(&block[0..4], &0x970b_76abu32.to_be_bytes());
        assert_eq!(block[4], (3 * 256 / 10) as u8, "fraction lost");
        assert_eq!(&block[5..8], &[0, 0, 3], "cumulative lost");
        assert_eq!(&block[8..12], &109u32.to_be_bytes(), "extended highest sequence");
        assert_eq!(&block[16..20], &0x1234_5678u32.to_be_bytes(), "LSR");
        let dlsr = u32::from_be_bytes(block[20..24].try_into().unwrap());
        assert!((3270..=3290).contains(&dlsr), "DLSR {dlsr}: 50 ms in 1/65536 s");
        // A steady stream has (almost) no jitter.
        assert!(u32::from_be_bytes(block[12..16].try_into().unwrap()) < 10);
    }

    #[test]
    fn the_sequence_wraps() {
        let start = Instant::now();
        let mut s = ReceiverStats::new(1, 48_000, 65_534);
        for (i, seq) in [65_534u16, 65_535, 0, 1].into_iter().enumerate() {
            s.received(seq, 480 * i as u32, start + Duration::from_millis(10 * i as u64));
        }
        assert_eq!(s.extended_max(), 65_536 + 1);
        let block = s.report_block(start);
        assert_eq!(&block[5..8], &[0, 0, 0], "nothing lost across the wrap");
    }
}
