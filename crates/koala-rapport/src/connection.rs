//! A Rapport connection over any byte stream (TCP, normally).

use apple_opack::Value;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::RapportError;
use crate::cipher::Cipher;
use crate::frame::{FrameType, HEADER_LEN, header, payload_len};

/// One end of a Rapport connection: frames in and out, encrypted once
/// pair-verify has given it a [`Cipher`].
pub struct Connection<S> {
    stream: S,
    cipher: Option<Cipher>,
    trace: Option<String>,
}

impl<S: AsyncRead + AsyncWrite + Unpin> Connection<S> {
    pub fn new(stream: S) -> Self {
        Self { stream, cipher: None, trace: None }
    }

    /// Prints every frame to standard error, after `label`: its type, size
    /// and (for OPACK frames) its decoded contents. For finding out what a
    /// peer sends, as in Koala's first trials with an iPhone.
    pub fn with_trace(mut self, label: impl Into<String>) -> Self {
        self.trace = Some(label.into());
        self
    }

    /// Adds a line to the trace, if there is one.
    pub fn note(&self, text: &str) {
        if let Some(label) = &self.trace {
            eprintln!("{label} {text}");
        }
    }

    fn trace(&self, arrow: &str, kind: FrameType, len: usize, value: Option<&Value>) {
        let Some(label) = &self.trace else { return };
        let encrypted = if self.cipher.is_some() { " encrypted" } else { "" };
        match value {
            Some(value) => eprintln!("{label} {arrow} frame {}, {len} bytes{encrypted}: {value:?}", kind.0),
            None => eprintln!("{label} {arrow} frame {}, {len} bytes{encrypted}", kind.0),
        }
    }

    /// Encrypts every frame from now on.
    pub fn encrypt(&mut self, cipher: Cipher) {
        self.cipher = Some(cipher);
    }

    pub fn is_encrypted(&self) -> bool {
        self.cipher.is_some()
    }

    pub async fn send(&mut self, kind: FrameType, payload: &[u8]) -> Result<(), RapportError> {
        let frame = match &mut self.cipher {
            Some(cipher) => cipher.seal(kind, payload),
            None => [&header(kind, payload.len())[..], payload].concat(),
        };
        self.stream.write_all(&frame).await?;
        self.stream.flush().await?;
        Ok(())
    }

    /// The next frame. A clean end of the stream is [`RapportError::Closed`].
    pub async fn receive(&mut self) -> Result<(FrameType, Vec<u8>), RapportError> {
        let mut header = [0; HEADER_LEN];
        match self.stream.read_exact(&mut header).await {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Err(RapportError::Closed),
            Err(e) => return Err(e.into()),
        }
        let mut payload = vec![0; payload_len(&header)];
        self.stream.read_exact(&mut payload).await?;
        if let Some(cipher) = &mut self.cipher {
            payload = cipher.open(&header, &payload)?;
        }
        Ok((FrameType(header[0]), payload))
    }

    pub async fn send_opack(&mut self, kind: FrameType, value: &Value) -> Result<(), RapportError> {
        let payload = apple_opack::pack(value)?;
        self.trace("->", kind, payload.len(), Some(value));
        self.send(kind, &payload).await
    }

    /// The next frame, its payload decoded as OPACK (an empty payload is an
    /// empty dictionary).
    pub async fn receive_opack(&mut self) -> Result<(FrameType, Value), RapportError> {
        let (kind, payload) = self.receive().await?;
        if payload.is_empty() {
            return Ok((kind, Value::Dict(Vec::new())));
        }
        let (value, _rest) = match apple_opack::unpack(&payload) {
            Ok(decoded) => decoded,
            Err(e) => {
                self.trace("<-", kind, payload.len(), None);
                return Err(e.into());
            }
        };
        self.trace("<-", kind, payload.len(), Some(&value));
        Ok((kind, value))
    }

    /// The stream underneath.
    pub fn get_ref(&self) -> &S {
        &self.stream
    }
}
