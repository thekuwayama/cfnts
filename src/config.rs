use std::fs;
use std::io::BufReader;

use config::Config;

use rustls::{
    internal::pemfile::{certs, pkcs8_private_keys, rsa_private_keys},
    Certificate, PrivateKey,
};

#[derive(Clone, Debug)]
pub struct MetricsConfig {
    pub port: u16,
    pub addr: String,
}

#[derive(Clone, Debug)]
pub struct ConfigNTSKE {
    pub tls_certs: Vec<Certificate>,
    pub tls_keys: Vec<PrivateKey>,
    pub cookie_key: Vec<u8>,
    pub addrs: Vec<String>,
    pub next_port: u16,
    pub conn_timeout: Option<u64>,
    pub memcached_url: String,
    pub metrics: MetricsConfig,
}

#[derive(Clone, Debug)]
pub struct ConfigNTP {
    pub addrs: Vec<String>,
    pub cookie_key: Vec<u8>,
    pub memcached_url: String,
    pub metrics: MetricsConfig,
    pub upstream_addr: Option<(String, u16)>,
}

#[derive(Clone, Debug)]
pub struct ConfigNTSClient {
    pub host: String,
    pub port: u16,
    pub trusted_cert: Option<Certificate>,
    pub use_ipv6: Option<bool>,
}

fn load_tls_certs(path: String) -> Vec<Certificate> {
    certs(&mut BufReader::new(fs::File::open(path).unwrap())).unwrap()
}

fn load_tls_keys(path: String) -> Vec<PrivateKey> {
    let res = pkcs8_private_keys(&mut BufReader::new(fs::File::open(path).unwrap()));
    res.unwrap()
}

fn load_cookie_key(path: String) -> Vec<u8> {
    fs::read(path).expect("Unable to read file")
}

fn to_string(v1: Vec<config::Value>) -> Vec<String> {
    let mut ret = vec![];
    for val in v1 {
        ret.push(val.into_str().unwrap());
    }
    ret
}

pub fn parse_nts_ke_config(config_filename: &str) -> ConfigNTSKE {
    let mut settings = Config::default();
    settings
        .merge(config::File::with_name(config_filename))
        .unwrap();

    // All config filenames MUST be given with relative paths to where the server is run.
    // Or else cf-nts will try to open the file while in the incorrect directory.
    let tls_cert_filename = settings.get_str("tls_cert_file").unwrap();
    let tls_key_filename = settings.get_str("tls_key_file").unwrap();
    let cookie_key_filename = settings.get_str("cookie_key_file").unwrap();

    let config = ConfigNTSKE {
        tls_certs: load_tls_certs(tls_cert_filename),
        tls_keys: load_tls_keys(tls_key_filename),
        cookie_key: load_cookie_key(cookie_key_filename),
        memcached_url: settings.get_str("memc_url").unwrap_or("".to_string()),
        addrs: to_string(settings.get_array("addr").unwrap()),
        next_port: settings.get_int("next_port").unwrap() as u16,
        conn_timeout: match settings.get_int("conn_timeout") {
            Err(_) => None,
            Ok(val) => Some(val as u64),
        },
        metrics: MetricsConfig {
            port: settings.get_int("metrics_port").unwrap() as u16,
            addr: settings.get_str("metrics_addr").unwrap(),
        },
    };
    config
}

pub fn parse_ntp_config(config_filename: &str) -> ConfigNTP {
    let mut settings = Config::default();
    settings
        .merge(config::File::with_name(config_filename))
        .unwrap();

    // All config filenames MUST be given with relative paths to where the server is run.
    // Or else cf-nts will try to open the file while in the incorrect directory.
    let cookie_key_filename = settings.get_str("cookie_key_file").unwrap();

    let config = ConfigNTP {
        cookie_key: load_cookie_key(cookie_key_filename),
        addrs: to_string(settings.get_array("addr").unwrap()),
        memcached_url: settings.get_str("memc_url").unwrap_or("".to_string()),
        metrics: MetricsConfig {
            port: settings.get_int("metrics_port").unwrap() as u16,
            addr: settings.get_str("metrics_addr").unwrap(),
        },
        upstream_addr: {
            match settings.get_str("upstream_host") {
                Ok(host) => match settings.get_int("upstream_port") {
                    Ok(port) => Some((host, port as u16)),
                    Err(_) => None,
                },
                Err(_) => None,
            }
        },
    };
    config
}

pub fn parse_nts_client_config(config_filename: &str) -> ConfigNTSClient {
    let mut settings = Config::default();
    settings
        .merge(config::File::with_name(config_filename))
        .unwrap();
    let config = ConfigNTSClient {
        host: settings.get_str("host").unwrap(),
        port: settings.get_int("port").unwrap() as u16,
        trusted_cert: match settings.get_str("trusted_certificate") {
            Err(_) => None,
            Ok(file) => Some(load_tls_certs(file)[0].clone()),
        },
        use_ipv6: match settings.get_bool("use_ipv6") {
            Err(_) => None,
            Ok(res) => Some(res),
        },
    };
    config
}
