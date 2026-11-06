//! The server side of SOCKS 4, 4a and 5, without authentication: enough to learn where a local
//! program wants to go. Only "connect" exists, as in OpenSSH's `DynamicForward`.
use anyhow::{Result, bail};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

const V4: u8 = 4;
const V5: u8 = 5;
const CONNECT: u8 = 1;

/// A name or a user id longer than this is not a real request.
const MAX_FIELD: usize = 512;

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Request {
    pub version: u8,
    pub host: String,
    pub port: u16,
}

async fn until_nul<S: AsyncRead + Unpin>(stream: &mut S) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    loop {
        match stream.read_u8().await? {
            0 => return Ok(bytes),
            byte if bytes.len() < MAX_FIELD => bytes.push(byte),
            _ => bail!("SOCKS field too long"),
        }
    }
}

async fn request_v4<S: AsyncRead + AsyncWrite + Unpin>(stream: &mut S) -> Result<Request> {
    let command = stream.read_u8().await?;
    let port = stream.read_u16().await?;
    let mut ip = [0; 4];
    stream.read_exact(&mut ip).await?;
    until_nul(stream).await?;
    // 0.0.0.x with x not zero: SOCKS 4a, and the name comes next.
    let host = if ip[..3] == [0, 0, 0] && ip[3] != 0 {
        String::from_utf8(until_nul(stream).await?)?
    } else {
        std::net::Ipv4Addr::from(ip).to_string()
    };
    if command != CONNECT {
        reply(stream, V4, false).await?;
        bail!("Unsupported SOCKS 4 command {command}");
    }
    Ok(Request {
        version: V4,
        host,
        port,
    })
}

async fn request_v5<S: AsyncRead + AsyncWrite + Unpin>(stream: &mut S) -> Result<Request> {
    let count = stream.read_u8().await?;
    let mut methods = vec![0; count as usize];
    stream.read_exact(&mut methods).await?;
    if !methods.contains(&0) {
        stream.write_all(&[V5, 0xff]).await?;
        bail!("The SOCKS client insists on authentication");
    }
    stream.write_all(&[V5, 0]).await?;
    let mut header = [0; 4];
    stream.read_exact(&mut header).await?;
    let [version, command, _, kind] = header;
    if version != V5 {
        bail!("Bad SOCKS 5 request");
    }
    let host = match kind {
        1 => {
            let mut ip = [0; 4];
            stream.read_exact(&mut ip).await?;
            std::net::Ipv4Addr::from(ip).to_string()
        }
        3 => {
            let len = stream.read_u8().await?;
            let mut name = vec![0; len as usize];
            stream.read_exact(&mut name).await?;
            String::from_utf8(name)?
        }
        4 => {
            let mut ip = [0; 16];
            stream.read_exact(&mut ip).await?;
            std::net::Ipv6Addr::from(ip).to_string()
        }
        _ => bail!("Unknown SOCKS 5 address type {kind}"),
    };
    let port = stream.read_u16().await?;
    if command != CONNECT {
        // "Command not supported".
        stream.write_all(&[V5, 7, 0, 1, 0, 0, 0, 0, 0, 0]).await?;
        bail!("Unsupported SOCKS 5 command {command}");
    }
    Ok(Request {
        version: V5,
        host,
        port,
    })
}

/// Reads what the client asks for. It waits for `reply` to know how that went.
pub(crate) async fn accept<S: AsyncRead + AsyncWrite + Unpin>(stream: &mut S) -> Result<Request> {
    match stream.read_u8().await? {
        V4 => request_v4(stream).await,
        V5 => request_v5(stream).await,
        version => bail!("Not a SOCKS client (version {version})"),
    }
}

pub(crate) async fn reply<S: AsyncWrite + Unpin>(
    stream: &mut S,
    version: u8,
    success: bool,
) -> Result<()> {
    if version == V4 {
        let status = if success { 90 } else { 91 };
        stream.write_all(&[0, status, 0, 0, 0, 0, 0, 0]).await?;
    } else {
        // 5: "connection refused", the usual reason a channel is not opened.
        let status = if success { 0 } else { 5 };
        stream
            .write_all(&[V5, status, 0, 1, 0, 0, 0, 0, 0, 0])
            .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What `accept` makes of these bytes, and what it answered.
    async fn run(input: &[u8]) -> (Result<Request>, Vec<u8>) {
        let (mut client, mut server) = tokio::io::duplex(4096);
        client.write_all(input).await.unwrap();
        let result = accept(&mut server).await;
        drop(server);
        let mut answer = Vec::new();
        client.read_to_end(&mut answer).await.unwrap();
        (result, answer)
    }

    #[tokio::test]
    async fn socks5_with_a_name() {
        let mut input = vec![5, 2, 0, 2, 5, 1, 0, 3, 11];
        input.extend(b"example.com");
        input.extend([1, 187]);
        let (request, answer) = run(&input).await;
        assert_eq!(
            request.unwrap(),
            Request {
                version: 5,
                host: "example.com".into(),
                port: 443
            }
        );
        assert_eq!(answer, [5, 0]);
    }

    #[tokio::test]
    async fn socks5_with_addresses() {
        let (request, _) = run(&[5, 1, 0, 5, 1, 0, 1, 10, 0, 0, 1, 0, 80]).await;
        assert_eq!(request.unwrap().host, "10.0.0.1");
        let mut input = vec![5, 1, 0, 5, 1, 0, 4];
        input.extend(std::net::Ipv6Addr::LOCALHOST.octets());
        input.extend([0, 80]);
        let (request, _) = run(&input).await;
        assert_eq!(request.unwrap().host, "::1");
    }

    #[tokio::test]
    async fn socks5_refuses_what_it_cannot_do() {
        let (request, answer) = run(&[5, 1, 2]).await;
        assert!(request.is_err());
        assert_eq!(answer, [5, 0xff]);
        // "Bind" instead of "connect".
        let (request, answer) = run(&[5, 1, 0, 5, 2, 0, 1, 10, 0, 0, 1, 0, 80]).await;
        assert!(request.is_err());
        assert_eq!(answer[..4], [5, 0, 5, 7]);
    }

    #[tokio::test]
    async fn socks4_with_an_address_and_4a_with_a_name() {
        let (request, _) = run(&[4, 1, 0, 80, 10, 0, 0, 2, b'u', 0]).await;
        assert_eq!(
            request.unwrap(),
            Request {
                version: 4,
                host: "10.0.0.2".into(),
                port: 80
            }
        );
        let mut input = vec![4, 1, 0, 80, 0, 0, 0, 1, 0];
        input.extend(b"box.local\0");
        let (request, _) = run(&input).await;
        assert_eq!(request.unwrap().host, "box.local");
    }

    #[tokio::test]
    async fn anything_else_is_not_a_client() {
        let (request, _) = run(b"GET / HTTP/1.1\r\n").await;
        assert!(request.is_err());
    }

    #[tokio::test]
    async fn replies_have_the_right_shape() {
        for (version, success, expected) in [
            (4, true, vec![0, 90, 0, 0, 0, 0, 0, 0]),
            (4, false, vec![0, 91, 0, 0, 0, 0, 0, 0]),
            (5, true, vec![5, 0, 0, 1, 0, 0, 0, 0, 0, 0]),
            (5, false, vec![5, 5, 0, 1, 0, 0, 0, 0, 0, 0]),
        ] {
            let mut out = Vec::new();
            reply(&mut out, version, success).await.unwrap();
            assert_eq!(out, expected);
        }
    }
}
