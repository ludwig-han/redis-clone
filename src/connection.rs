//! 연결별 스레드와 TCP 수신 버퍼를 관리합니다.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::thread;

use crate::command::build_reply;
use crate::db::Store;
use crate::resp::{Parsed, parse_command};

pub(super) fn accept_connections(listener: TcpListener, store: Store) {
    for incoming in listener.incoming() {
        match incoming {
            // 커넥션 하나당 스레드 하나. 한 커넥션이 막혀도 다른 커넥션은 계속 처리됩니다.
            Ok(stream) => {
                // 같은 실제 데이터를 가리키는 '손잡이'를 하나 더 만듭니다. Arc::clone 은 데이터를 복사하지 않고, Arc 내부의 카운터만 증가시킵니다.
                let store = Arc::clone(&store);

                thread::spawn(move || handle_connection(stream, store));
            }
            // 연결 하나를 못 받았다고 서버를 끄지 않습니다.
            Err(error) => eprintln!("연결을 수락하지 못했습니다: {error}"),
        }
    }
}

fn handle_connection(mut stream: TcpStream, store: Store) {
    // 읽은 바이트를 모아두는 버퍼입니다.
    // TCP는 메시지 단위가 아니라 바이트 흐름이라, 명령 하나가 여러 번에 나눠 도착할 수 있습니다.
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 1024];

    loop {
        let read = match stream.read(&mut chunk) {
            Ok(0) => return, // 상대가 연결을 닫았습니다
            Ok(count) => count,
            Err(error) => {
                eprintln!("읽지 못했습니다: {error}");
                return;
            }
        };
        buffer.extend_from_slice(&chunk[..read]);

        loop {
            let (command, consumed) = match parse_command(&buffer) {
                Parsed::Complete(command, consumed) => (command, consumed),
                Parsed::Incomplete => break, // 뒷부분이 아직 안 왔습니다. 더 읽습니다
                // 형식이 깨지면 연결을 끊습니다. 진짜 Redis는 구체적인 문구도 함께 보냅니다.
                Parsed::Broken => return,
            };
            buffer.drain(..consumed);

            let reply = build_reply(&command, &store);
            if reply.is_empty() {
                continue;
            }
            if let Err(error) = stream.write_all(&reply) {
                eprintln!("쓰지 못했습니다: {error}");
                return;
            }
        }
    }
}
