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
}

impl<S: AsyncRead + AsyncWrite + Unpin> Connection<S> {
    pub fn new(stream: S) -> Self {
        Self { stream, cipher: None }
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
        self.send(kind, &apple_opack::pack(value)?).await
    }

    /// The next frame, its payload decoded as OPACK (an empty payload is an
    /// empty dictionary).
    pub async fn receive_opack(&mut self) -> Result<(FrameType, Value), RapportError> {
        let (kind, payload) = self.receive().await?;
        if payload.is_empty() {
            return Ok((kind, Value::Dict(Vec::new())));
        }
        let (value, _rest) = apple_opack::unpack(&payload)?;
        Ok((kind, value))
    }

    /// The stream underneath.
    pub fn get_ref(&self) -> &S {
        &self.stream
    }
}
