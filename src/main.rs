mod command;
mod connection;
mod db;
mod number;
mod reply;
mod resp;

use std::collections::HashMap;
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

use connection::accept_connections;
use db::Store;

/// 진짜 Redis가 6379를 쓰기 때문에 우리는 6380을 씁니다.
const DEFAULT_PORT: u16 = 6380;

fn main() {
    let port = match port_from_args() {
        Ok(port) => port,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(1);
        }
    };

    let listener = match TcpListener::bind(("127.0.0.1", port)) {
        Ok(listener) => listener,
        Err(error) => {
            eprintln!("포트 {port} 을(를) 열지 못했습니다: {error}");
            std::process::exit(1);
        }
    };

    // Arc: 여러 thread가 같은 데이터를 같이 가리킬 수 있게 해주는 공유용 포인터
    // Mutex: 한 번에 한 thread만 데이터를 바꿀 수 있게 해주는 잠금 장치
    let store: Store = Arc::new(Mutex::new(HashMap::new()));

    eprintln!("포트 {port} 에서 연결을 기다립니다");

    accept_connections(listener, store);
}

/// --port 1234 같은 인자를 처리합니다. 없으면 DEFAULT_PORT를 씁니다.
fn port_from_args() -> Result<u16, String> {
    let mut args = std::env::args().skip(1);
    let mut port = DEFAULT_PORT;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--port" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--port 뒤에 포트 번호가 없습니다".to_string())?;
                port = value
                    .parse()
                    .map_err(|_| format!("포트 번호가 잘못되었습니다: {value}"))?;
            }
            other => return Err(format!("모르는 인자입니다: {other}")),
        }
    }

    Ok(port)
}
