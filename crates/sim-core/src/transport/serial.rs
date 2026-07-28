use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_serial::{DataBits, FlowControl, Parity, SerialPortBuilderExt, SerialStream, StopBits};

use super::{Received, Transport};
use crate::error::TransportError;

const RECV_BUFFER_SIZE: usize = 4096;

pub struct SerialTransport {
    port: SerialStream,
    buf: Vec<u8>,
}

impl SerialTransport {
    /// # Errors
    ///
    /// Returns an error if `port_name` cannot be opened with the given settings.
    pub fn open(
        port_name: &str,
        baud_rate: u32,
        data_bits: DataBits,
        parity: Parity,
        stop_bits: StopBits,
        flow_control: FlowControl,
    ) -> Result<Self, TransportError> {
        let port = tokio_serial::new(port_name, baud_rate)
            .data_bits(data_bits)
            .parity(parity)
            .stop_bits(stop_bits)
            .flow_control(flow_control)
            .open_native_async()
            .map_err(|source| TransportError::SerialOpen {
                port: port_name.to_owned(),
                source,
            })?;
        Ok(Self {
            port,
            buf: vec![0u8; RECV_BUFFER_SIZE],
        })
    }
}

impl Transport for SerialTransport {
    async fn send(&mut self, bytes: &[u8]) -> Result<(), TransportError> {
        self.port.write_all(bytes).await?;
        Ok(())
    }

    async fn recv(&mut self) -> Result<Received, TransportError> {
        let n = self.port.read(&mut self.buf).await?;
        if n == 0 {
            return Err(TransportError::Closed);
        }
        Ok(Received {
            bytes: self.buf[..n].to_vec(),
            source: None,
        })
    }
}
