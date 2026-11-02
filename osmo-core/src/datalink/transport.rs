//! DUML-over-UDP datalink: socket-agnostic framing, sequencing and the sliding-window ack.
//! Wire layers per packet: `[8B transport hdr][12B routing hdr][DUML frame]`. Ported from Osmosis
//! `DumlTransport.kt`; its doc comments explain why each sequence field holds what it holds
//! (two past outages were wrong values in the routing header).
use crate::duml::Frame;
use rand::Rng;
use std::io;
use std::net::{SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

pub trait Datagram {
    fn send(&mut self, pkt: &[u8]) -> io::Result<()>;
    /// Next datagram, or `None` if nothing arrived within `timeout`.
    fn recv(&mut self, timeout: Duration) -> io::Result<Option<Vec<u8>>>;
}

pub struct UdpPeer {
    sock: UdpSocket,
    peer: SocketAddr,
}

impl UdpPeer {
    pub fn connect(ip: &str, port: u16) -> io::Result<Self> {
        let peer: SocketAddr = format!("{ip}:{port}").parse().map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("bad address {ip}:{port}: {e}"),
            )
        })?;
        Ok(UdpPeer {
            sock: UdpSocket::bind("0.0.0.0:0")?,
            peer,
        })
    }
}

impl Datagram for UdpPeer {
    fn send(&mut self, pkt: &[u8]) -> io::Result<()> {
        self.sock.send_to(pkt, self.peer).map(|_| ())
    }

    fn recv(&mut self, timeout: Duration) -> io::Result<Option<Vec<u8>>> {
        self.sock
            .set_read_timeout(Some(timeout.max(Duration::from_millis(1))))?;
        let mut buf = vec![0u8; 65536];
        loop {
            match self.sock.recv_from(&mut buf) {
                Ok((n, from)) if from.ip() == self.peer.ip() => return Ok(Some(buf[..n].to_vec())),
                Ok(_) => continue,
                Err(e)
                    if matches!(
                        e.kind(),
                        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                    ) =>
                {
                    return Ok(None)
                }
                Err(e) => return Err(e),
            }
        }
    }
}

/// 8-byte transport header: `[u16 flag|total][u16 session][u16 seq][u8 pktType][u8 xor of the rest]`.
pub fn udp_header(pkt_type: u8, payload_len: usize, session: u16, seq: u16) -> [u8; 8] {
    let w0 = (1u16 << 15) | ((8 + payload_len) as u16 & 0x3FFF);
    let mut b = [0u8; 8];
    b[0..2].copy_from_slice(&w0.to_le_bytes());
    b[2..4].copy_from_slice(&session.to_le_bytes());
    b[4..6].copy_from_slice(&seq.to_le_bytes());
    b[6] = pkt_type;
    b[7] = b[..7].iter().fold(0, |a, x| a ^ x);
    b
}

/// 12-byte command routing header: `[ack = my previous seq][my seq]` in OUR OWN seq space.
pub fn routing_header(seq: u16, cmd_counter: u8, drone: bool) -> [u8; 12] {
    let ack = seq.wrapping_sub(8);
    let mut b = [0u8; 12];
    b[0..2].copy_from_slice(&ack.to_le_bytes());
    b[2..4].copy_from_slice(&seq.to_le_bytes());
    b[8] = cmd_counter;
    b[9] = 0x01;
    b[10] = if drone { 0x60 } else { 0x00 };
    b
}

pub struct Transport<D: Datagram> {
    d: D,
    session: u16,
    base_seq: u16,
    udp_seq: u16,
    duml_seq: u16,
    cmd_counter: u8,
    cam_channel: u16,
    peer_cursor: u16,
    peer_download_cursor: u16,
    /// How long each handshake attempt listens for a reply.
    pub handshake_wait: Duration,
}

impl<D: Datagram> Transport<D> {
    pub fn new(d: D, base_seq: u16, session: u16) -> Self {
        Transport {
            d,
            session,
            base_seq,
            udp_seq: 0,
            duml_seq: 0xA000,
            cmd_counter: 0,
            cam_channel: base_seq,
            peer_cursor: 0,
            peer_download_cursor: 0,
            handshake_wait: Duration::from_millis(350),
        }
    }

    /// Fresh session id and 8-aligned base sequence per connect: reusing a fixed base can leave the
    /// peer's session state wedged.
    pub fn random(d: D) -> Self {
        let mut r = rand::thread_rng();
        Self::new(
            d,
            r.gen_range(0x1000..0xF000u16) & 0xFFF8,
            r.gen_range(0x1000..0xFFFEu16),
        )
    }

    pub fn dg(&mut self) -> &mut D {
        &mut self.d
    }
    pub fn seq(&self) -> u16 {
        self.udp_seq
    }
    pub fn base_seq(&self) -> u16 {
        self.base_seq
    }
    pub fn session(&self) -> u16 {
        self.session
    }
    pub fn peer_channel(&self) -> u16 {
        self.cam_channel
    }

    fn send_packet(&mut self, pkt: &[u8]) -> bool {
        self.d.send(pkt).is_ok()
    }

    fn advance(&mut self) {
        self.udp_seq = self.udp_seq.wrapping_add(8);
    }

    pub fn send_raw(&mut self, pkt_type: u8, payload: &[u8]) {
        let mut pkt = udp_header(pkt_type, payload.len(), self.session, self.udp_seq).to_vec();
        pkt.extend_from_slice(payload);
        if self.send_packet(&pkt) {
            self.advance();
        }
    }

    /// Send pktType-0 frames until the peer answers with one; `None` after `attempts` tries.
    pub fn handshake(&mut self, payload: &[u8], attempts: u32) -> Option<Vec<u8>> {
        for _ in 0..attempts {
            self.send_raw(0x00, payload);
            let wait = self.handshake_wait;
            if let Some(r) = self
                .recv_all(wait)
                .into_iter()
                .find(|r| r.len() >= 8 && r[6] == 0x00)
            {
                return Some(r);
            }
        }
        None
    }

    pub fn sync_seq_to_peer_channel(&mut self) {
        self.udp_seq = self.cam_channel.wrapping_add(8);
    }

    /// pktType-0x04 window acknowledgement: three `[start][end][zero]` groups (video, download,
    /// control) plus a trailing `[u16 0]`, sent with seq 0. Echoing the peer's own cursors is what
    /// keeps it streaming; the control group stays at the base.
    pub fn send_ack(&mut self) {
        fn grp(v: u16) -> [u8; 8] {
            let [lo, hi] = v.to_le_bytes();
            [lo, hi, lo, hi, 0, 0, 0, 0]
        }
        let mut payload = Vec::with_capacity(26);
        payload.extend(grp(self.peer_cursor));
        payload.extend(grp(self.peer_download_cursor));
        payload.extend(grp(self.base_seq));
        payload.extend([0, 0]);
        let mut pkt = udp_header(0x04, payload.len(), self.session, 0).to_vec();
        pkt.extend(payload);
        self.send_packet(&pkt);
    }

    pub fn send_duml(
        &mut self,
        set: u8,
        cmd: u8,
        payload: &[u8],
        recv_type: u8,
        recv_id: u8,
        cmd_type: u8,
    ) {
        self.cmd_counter = self.cmd_counter.wrapping_add(1);
        let rt = routing_header(self.udp_seq, self.cmd_counter, false);
        let target = 0x02 | ((((recv_id as u16) << 5) | recv_type as u16) << 8);
        let ty = ((cmd_type as u32) << 5) | (set as u32) << 8 | (cmd as u32) << 16;
        let duml = Frame {
            target,
            id: self.duml_seq,
            ty,
            payload: payload.to_vec(),
        }
        .encode();
        self.duml_seq = self.duml_seq.wrapping_add(1);
        let mut pkt = udp_header(0x05, rt.len() + duml.len(), self.session, self.udp_seq).to_vec();
        pkt.extend(rt);
        pkt.extend(duml);
        if self.send_packet(&pkt) {
            self.advance();
        }
    }

    /// Everything that arrives in `dur`, latching the peer's channel (bytes 8-9 of every packet)
    /// and its window cursors (34-byte pktType-1 status frames: video `[10:12]`, download `[18:20]`).
    pub fn recv_all(&mut self, dur: Duration) -> Vec<Vec<u8>> {
        let end = Instant::now() + dur;
        let mut out = Vec::new();
        loop {
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            match self.d.recv(left.min(Duration::from_millis(200))) {
                Ok(Some(data)) => {
                    if data.len() >= 10 {
                        let ch = u16::from_le_bytes([data[8], data[9]]);
                        if ch != 0 {
                            self.cam_channel = ch;
                        }
                    }
                    if data.len() == 34 && data[6] == 0x01 {
                        self.peer_cursor = u16::from_le_bytes([data[10], data[11]]);
                        self.peer_download_cursor = u16::from_le_bytes([data[18], data[19]]);
                    }
                    out.push(data);
                }
                Ok(None) => {}
                Err(_) => break,
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::duml::Frame;
    use std::collections::VecDeque;
    use std::io;
    use std::time::Duration;

    #[derive(Default)]
    struct Fake {
        sent: Vec<Vec<u8>>,
        inbox: VecDeque<Vec<u8>>,
    }
    impl Datagram for Fake {
        fn send(&mut self, pkt: &[u8]) -> io::Result<()> {
            self.sent.push(pkt.to_vec());
            Ok(())
        }
        fn recv(&mut self, timeout: Duration) -> io::Result<Option<Vec<u8>>> {
            if let Some(x) = self.inbox.pop_front() {
                return Ok(Some(x));
            }
            std::thread::sleep(timeout.min(Duration::from_millis(2)));
            Ok(None)
        }
    }
    fn tx() -> Transport<Fake> {
        let mut t = Transport::new(Fake::default(), 0x87B8, 0x2965);
        t.handshake_wait = Duration::from_millis(10);
        t
    }
    fn status34(video_end: u16, download_end: u16) -> Vec<u8> {
        let mut p = vec![0u8; 34];
        p[6] = 0x01;
        p[10..12].copy_from_slice(&video_end.to_le_bytes());
        p[18..20].copy_from_slice(&download_end.to_le_bytes());
        p
    }

    #[test]
    fn transport_header_carries_session_and_seq_with_xor_trailer() {
        let h = udp_header(0x05, 40, 0x2965, 0x87C0);
        assert_eq!(h.len(), 8);
        assert_eq!(
            (h[2], h[3], h[4], h[5], h[6]),
            (0x65, 0x29, 0xC0, 0x87, 0x05)
        );
        assert_eq!(h[7], h[..7].iter().fold(0, |a, b| a ^ b));
    }

    #[test]
    fn total_length_is_payload_plus_eight_with_flag_bit() {
        let h = udp_header(0x05, 40, 0x2965, 0);
        let w0 = u16::from_le_bytes([h[0], h[1]]);
        assert_eq!(w0 & 0x3FFF, 48);
        assert_ne!(w0 & 0x8000, 0);
    }

    #[test]
    fn routing_ack_is_our_previous_seq_and_wraps() {
        let rt = routing_header(0x1570, 3, false);
        assert_eq!(rt.len(), 12);
        assert_eq!(u16::from_le_bytes([rt[2], rt[3]]), 0x1570);
        assert_eq!(u16::from_le_bytes([rt[0], rt[1]]), 0x1568);
        assert_eq!(
            u16::from_le_bytes([
                routing_header(4, 0, false)[0],
                routing_header(4, 0, false)[1]
            ]),
            0xFFFC
        );
    }

    #[test]
    fn routing_byte_10_marks_drone_and_nothing_else_differs() {
        let (d, c) = (
            routing_header(0x100, 7, true),
            routing_header(0x100, 7, false),
        );
        assert_eq!((d[10], c[10]), (0x60, 0x00));
        assert_eq!(d[..10], c[..10]);
        assert_eq!((d[8], d[9]), (7, 0x01));
    }

    #[test]
    fn seq_advances_by_8_per_sent_packet() {
        let mut t = tx();
        t.send_raw(0x00, &[1]);
        t.send_raw(0x00, &[1]);
        assert_eq!(t.seq(), 16);
    }

    #[test]
    fn send_duml_layout_and_target_encoding() {
        let mut t = tx();
        t.send_duml(0x00, 0x26, &[1, 2], 0x01, 0, 2);
        let pkt = &t.dg().sent[0];
        assert_eq!(pkt[6], 0x05);
        let f = Frame::decode(&pkt[20..]).unwrap();
        assert_eq!(
            (f.target, f.ty, f.payload.clone()),
            (0x0102, 0x40 | 0x26 << 16, vec![1, 2])
        );
        assert_eq!(
            u16::from_le_bytes([pkt[0], pkt[1]]) & 0x3FFF,
            pkt.len() as u16
        );
        assert_eq!(pkt[8 + 8], 1, "command counter in the routing header");
    }

    #[test]
    fn duml_ids_increment_from_0xa000() {
        let mut t = tx();
        t.send_duml(0, 0x88, &[], 0x08, 1, 2);
        t.send_duml(0, 0x88, &[], 0x08, 1, 2);
        let ids: Vec<u16> = t
            .dg()
            .sent
            .iter()
            .map(|p| Frame::decode(&p[20..]).unwrap().id)
            .collect();
        assert_eq!(ids, vec![0xA000, 0xA001]);
    }

    #[test]
    fn ack_echoes_download_and_video_cursors_and_keeps_seq_zero() {
        let mut t = tx();
        t.dg().inbox.push_back(status34(0x0BCD, 0x1234));
        t.recv_all(Duration::from_millis(5));
        let before = t.seq();
        t.send_ack();
        let pkt = t.dg().sent.last().unwrap().clone();
        assert_eq!(pkt.len(), 34, "8-byte header + 26-byte payload");
        assert_eq!(pkt[6], 0x04);
        assert_eq!(u16::from_le_bytes([pkt[4], pkt[5]]), 0, "ack carries seq 0");
        let p = &pkt[8..];
        assert_eq!(&p[0..4], &[0xCD, 0x0B, 0xCD, 0x0B]);
        assert_eq!(&p[8..12], &[0x34, 0x12, 0x34, 0x12]);
        assert_eq!(
            &p[16..20],
            &[0xB8, 0x87, 0xB8, 0x87],
            "control group stays at the base"
        );
        assert_eq!(t.seq(), before, "an ack does not advance our send seq");
    }

    #[test]
    fn recv_all_latches_peer_channel_and_ignores_zero() {
        let mut t = tx();
        let mut a = vec![0u8; 12];
        a[8] = 0x34;
        a[9] = 0x12;
        t.dg().inbox.push_back(a);
        t.dg().inbox.push_back(vec![0u8; 12]);
        assert_eq!(t.recv_all(Duration::from_millis(5)).len(), 2);
        assert_eq!(t.peer_channel(), 0x1234);
        t.sync_seq_to_peer_channel();
        assert_eq!(t.seq(), 0x123C);
    }

    #[test]
    fn handshake_retries_until_a_type_zero_reply() {
        let mut t = tx();
        let mut reply = vec![0u8; 10];
        reply[6] = 0x00;
        t.dg().inbox.push_back(
            vec![0u8; 12]
                .into_iter()
                .enumerate()
                .map(|(i, b)| if i == 6 { 0x05 } else { b })
                .collect(),
        );
        t.dg().inbox.push_back(reply.clone());
        assert_eq!(t.handshake(&[9, 9], 5), Some(reply));
        assert_eq!(t.dg().sent[0][6], 0x00);
    }

    #[test]
    fn handshake_gives_up_after_attempts() {
        let mut t = tx();
        assert_eq!(t.handshake(&[9], 3), None);
        assert_eq!(t.dg().sent.len(), 3);
    }
}
