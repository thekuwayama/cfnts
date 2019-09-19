use slog::{debug, info};
use std::error::Error;
use std::fmt;
use std::io::{Read, Write};
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;

use rustls;
use webpki;
use webpki_roots;

use super::protocol;
use super::protocol::{DeserializeError::TooShort, *};

use self::ClientError::*;
use crate::client::ClientConfig;
use crate::cookie::NTSKeys;

type Cookie = Vec<u8>;

const DEFAULT_NTP_PORT: u16 = 123;
const DEFAULT_KE_PORT: u16 = 1234;
const DEFAULT_SCHEME: u16 = 0;
const TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Clone, Debug)]
struct ClientState {
    finished: bool,
    cookies: Vec<Cookie>,
    next_protocols: Vec<u16>,
    aead_scheme: u16,
    next_port: u16,
    next_server: String,
    keys: NTSKeys,
}

#[derive(Clone, Debug)]
pub struct NtsKeResult {
    pub cookies: Vec<Cookie>,
    pub next_protocols: Vec<u16>,
    pub aead_scheme: u16,
    pub next_server: String,
    pub next_port: u16,
    pub keys: NTSKeys,
    pub use_ipv4: Option<bool>
}

#[derive(Debug, Clone)]
pub enum ClientError {
    RecordAfterEnd,
    ErrorRecord,
    InvalidRecord,
    NoIpv4AddrFound,
    NoIpv6AddrFound,
}

impl std::error::Error for ClientError {
    fn description(&self) -> &str {
        match self {
            _ => "Something is wrong",
        }
    }
    fn cause(&self) -> Option<&std::error::Error> {
        None
    }
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Client Error")
    }
}

/// Read https://tools.ietf.org/html/draft-ietf-ntp-using-nts-for-ntp-19#section-4
fn process_record(
    rec: protocol::NtsKeRecord,
    state: &mut ClientState,
) -> Result<(), Box<dyn std::error::Error>> {
    if state.finished {
        return Err(Box::new(RecordAfterEnd));
    }
    match rec.record_type {
        NtsKeType::EndOfMessage => state.finished = true,
        NtsKeType::NextProtocolNegotiation => state.next_protocols = extract_protos(rec)?,
        NtsKeType::Error => return Err(Box::new(ErrorRecord)),
        NtsKeType::Warning => return Ok(()),
        NtsKeType::AEADAlgorithmNegotiation => {
            let schemes = extract_aead(rec)?;
            state.aead_scheme = schemes[0];
            if schemes.len() != 1 {
                return Err(Box::new(InvalidRecord));
            }
        }
        NtsKeType::NewCookie => state.cookies.push(rec.contents.clone()),
        NtsKeType::ServerNegotiation => return Ok(()), // not yet supported
        NtsKeType::PortNegotiation => state.next_port = extract_port(rec)?,
    }
    Ok(())
}

/// run_nts_client executes the nts client with the config in config file
pub fn run_nts_ke_client(
    logger: &slog::Logger,
    client_config: ClientConfig
) -> Result<NtsKeResult, Box<dyn Error>> {
    let mut tls_config = rustls::ClientConfig::new();
    let alpn_proto = String::from("ntske/1");
    let alpn_bytes = alpn_proto.into_bytes();
    tls_config.set_protocols(&[alpn_bytes]);

    match client_config.trusted_cert {
        Some(cert) => {
            info!(logger, "loading custom trust root");
            tls_config.root_store.add(&cert)?;
        }
        None => {
                tls_config
                .root_store
                .add_server_trust_anchors(&webpki_roots::TLS_SERVER_ROOTS);
        },
    }

    let rc_config = Arc::new(tls_config);
    let hostname = webpki::DNSNameRef::try_from_ascii_str(client_config.host.as_str())
        .expect("server hostname is invalid");
    let mut client = rustls::ClientSession::new(&rc_config, hostname);
    debug!(logger, "Connecting");
    let mut port = DEFAULT_KE_PORT;
    if let Some(p) = client_config.port {
        port = p.parse::<u16>()?;
    }

    let mut ip_addrs = (client_config.host.as_str(), port).to_socket_addrs()?;
    let addr;
    if let Some(use_ipv4) = client_config.use_ipv4 {
        if use_ipv4 {
            // mandated to use ipv4
            addr = ip_addrs.find(|&x| x.is_ipv4());
            if addr == None {
                return Err(Box::new(NoIpv4AddrFound));
            }
        } else {
            // mandated to use ipv6
            addr = ip_addrs.find(|&x| x.is_ipv6());
            if addr == None {
                return Err(Box::new(NoIpv6AddrFound));
            }
        }
    } else {
        // sniff whichever one is supported
        addr = ip_addrs.next();
    }
    let mut stream = TcpStream::connect_timeout(&addr.unwrap(), TIMEOUT)?;
    stream.set_read_timeout(Some(TIMEOUT))?;
    stream.set_write_timeout(Some(TIMEOUT))?;

    let mut tls_stream = rustls::Stream::new(&mut client, &mut stream);

    let mut next_proto = NtsKeRecord {
        critical: true,
        record_type: NtsKeType::NextProtocolNegotiation,
        contents: vec![0, 0],
    };

    let mut aead_rec = NtsKeRecord {
        critical: false,
        record_type: NtsKeType::AEADAlgorithmNegotiation,
        contents: vec![0, 15],
    };

    let mut end_rec = NtsKeRecord {
        critical: true,
        record_type: NtsKeType::EndOfMessage,
        contents: vec![],
    };

    tls_stream.write(&protocol::serialize_record(&mut next_proto))?;
    tls_stream.write(&protocol::serialize_record(&mut aead_rec))?;
    tls_stream.write(&protocol::serialize_record(&mut end_rec))?;
    tls_stream.flush()?;
    debug!(logger, "Request transmitted");
    let keys = protocol::gen_key(tls_stream.sess).unwrap();

    let mut state = ClientState {
        finished: false,
        cookies: Vec::new(),
        next_protocols: Vec::new(),
        next_server: client_config.host.clone(),
        next_port: DEFAULT_NTP_PORT,
        keys: keys,
        aead_scheme: DEFAULT_SCHEME,
    };

    let mut curr = 0;
    let mut readptr = 0;
    let mut buf = vec![0; 4]; // start with a header
    while state.finished == false {
        // We now read records from the server and process them.
        // Buf contains all the data the server sent us. curr points at the last processed
        // record, readptr points at the last read data.
        let more = tls_stream.read(&mut buf[readptr..]);
        if let Err(err) = more {
            return Err(Box::new(err));
        }
        readptr += more.unwrap();
        loop {
            // We've read some data, let's see if we get further with it.
            // This loop reads either 1 or 0 records each time.
            // It's structured as a loop because reading from an empty buffer
            // and reading from an insufficiently long buffer both work the same
            // way. We have no promises enough was read.
            let rec = protocol::deserialize_record(&buf[curr..]);
            match rec {
                Ok((Some(rec), len)) => {
                    debug!(logger, "Record: {:?}", rec);
                    let status = process_record(rec, &mut state);
                    match status {
                        Ok(_) => {}
                        Err(err) => return Err(err),
                    }
                    curr += len;
                }
                Ok((None, len)) => {
                    debug!(logger, "Unknown record type");
                    curr += len;
                }
                Err(err) => match err {
                    TooShort(n) => {
                        debug!(logger, "minimum length {:}", n);
                        buf.resize(curr + n, 0);
                        // The buffer is now at least n bytes beyond curr.
                        break;
                    }
                    _ => {
                        debug!(logger, "error: {:?}", err);
                        return Err(Box::new(err));
                    }
                },
            }
        }
    }
    debug!(logger, "saw the end of the response");
    stream.shutdown(Shutdown::Both)?;

    Ok(NtsKeResult {
        aead_scheme: state.aead_scheme,
        cookies: state.cookies,
        next_protocols: state.next_protocols,
        next_server: state.next_server,
        next_port: state.next_port,
        keys: state.keys,
        use_ipv4: client_config.use_ipv4
    })
}
