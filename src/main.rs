use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;
use std::time::{Duration, Instant};

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
type Store = Arc<Mutex<HashMap<Vec<u8>, Entry>>>;

/// 진짜 Redis가 6379를 쓰기 때문에 우리는 6380을 씁니다.
const DEFAULT_PORT: u16 = 6380;

struct Entry {
    value: Vec<u8>,
    expires_at: Option<Instant>,
}

fn is_expired(entry: &Entry) -> bool {
    match entry.expires_at {
        Some(expires_at) => Instant::now() >= expires_at,
        None => false,
    }
}

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

/// 버퍼 앞쪽에서 명령 하나를 꺼낸 결과입니다.
enum Parsed {
    /// 명령 하나와, 그 명령이 버퍼에서 차지한 바이트 수
    Complete(Vec<Vec<u8>>, usize),
    /// 아직 덜 왔습니다
    Incomplete,
    /// 형식이 깨졌습니다
    Broken,
}

/// redis-cli 는 명령을 RESP 배열로 보냅니다: `*2\r\n$4\r\nPING\r\n$5\r\nhello\r\n`
/// 진짜 Redis는 `nc` 로 친 생 텍스트(인라인 명령)도 받아줍니다.
fn parse_command(buffer: &[u8]) -> Parsed {
    match buffer.first() {
        None => Parsed::Incomplete,
        Some(b'*') => parse_array(buffer),
        Some(_) => Parsed::Broken,
    }
}

// `*2\r\n$4\r\nPING\r\n$5\r\nhello\r\n` 같은 RESP 배열을 파싱합니다.
fn parse_array(buffer: &[u8]) -> Parsed {
    let Some((header, mut offset)) = read_line(buffer, 0) else {
        return Parsed::Incomplete;
    };
    let Some(count) = parse_number(&header[1..]) else {
        return Parsed::Broken;
    };
    if count < 0 {
        return Parsed::Broken;
    }

    // count 를 그대로 믿고 with_capacity 를 쓰면, 큰 숫자 하나로 메모리를 왕창 잡을 수 있습니다.
    let mut parts = Vec::new();

    for _ in 0..count {
        let Some((header, after_header)) = read_line(buffer, offset) else {
            return Parsed::Incomplete;
        };
        if header.first() != Some(&b'$') {
            return Parsed::Broken;
        }
        let Some(length) = parse_number(&header[1..]) else {
            return Parsed::Broken;
        };
        let Ok(length) = usize::try_from(length) else {
            return Parsed::Broken;
        };

        let end = after_header + length;
        // 본문 뒤의 \r\n 까지 도착해야 이 조각이 완성된 것입니다.
        if buffer.len() < end + 2 {
            return Parsed::Incomplete;
        }
        parts.push(buffer[after_header..end].to_vec());
        offset = end + 2;
    }

    Parsed::Complete(parts, offset)
}

/// `from` 위치부터 `\r\n` 직전까지를 잘라내고, 그 다음 위치를 같이 돌려줍니다.
fn read_line(buffer: &[u8], from: usize) -> Option<(&[u8], usize)> {
    let rest = buffer.get(from..)?;
    let end = rest.windows(2).position(|pair| pair == b"\r\n")?;
    Some((&rest[..end], from + end + 2))
}

fn parse_number(bytes: &[u8]) -> Option<i64> {
    std::str::from_utf8(bytes).ok()?.parse().ok()
}

// 명령어에 따라 적절한 응답을 만듭니다.
fn build_reply(command: &[Vec<u8>], store: &Store) -> Vec<u8> {
    // 빈 배열(`*0\r\n`)에는 진짜 Redis도 아무 응답을 하지 않습니다.
    let Some(name) = command.first() else {
        return Vec::new();
    };

    if name.eq_ignore_ascii_case(b"PING") {
        return match command.len() {
            1 => b"+PONG\r\n".to_vec(),
            2 => bulk_string(&command[1]),
            _ => error_reply(b"ERR wrong number of arguments for 'ping' command"),
        };
    }

    if name.eq_ignore_ascii_case(b"ECHO") {
        return match command.len() {
            2 => bulk_string(&command[1]),
            _ => error_reply(b"ERR wrong number of arguments for 'echo' command"),
        };
    }

    if name.eq_ignore_ascii_case(b"SET") {
        if command.len() < 3 {
            return error_reply(b"ERR wrong number of arguments for 'set' command");
        }

        let mut ttl: Option<Duration> = None;
        let mut nx = false;
        let mut xx = false;

        let mut i = 3;

        while i < command.len() {
            if command[i].eq_ignore_ascii_case(b"EX") {
                if ttl.is_some() || i + 1 >= command.len() {
                    return error_reply(b"ERR syntax error");
                }

                let seconds = match std::str::from_utf8(&command[i + 1])
                    .ok()
                    .and_then(|s| s.parse::<i64>().ok())
                {
                    Some(seconds) if seconds > 0 => seconds,
                    _ => return error_reply(b"ERR invalid expire time in 'set' command"),
                };

                ttl = Some(Duration::from_secs(seconds as u64));
                i += 2; // 다음 옵션으로 넘어갑니다.
            } else if command[i].eq_ignore_ascii_case(b"PX") {
                if ttl.is_some() || i + 1 >= command.len() {
                    return error_reply(b"ERR syntax error");
                }

                let milliseconds = match std::str::from_utf8(&command[i + 1])
                    .ok()
                    .and_then(|s| s.parse::<i64>().ok())
                {
                    Some(milliseconds) if milliseconds > 0 => milliseconds,
                    _ => return error_reply(b"ERR invalid expire time in 'set' command"),
                };

                ttl = Some(Duration::from_millis(milliseconds as u64));
                i += 2;
            } else if command[i].eq_ignore_ascii_case(b"NX") {
                if nx || xx {
                    // NX와 XX는 동시에 사용할 수 없습니다.
                    return error_reply(b"ERR syntax error");
                }

                nx = true;
                i += 1;
            } else if command[i].eq_ignore_ascii_case(b"XX") {
                if nx || xx {
                    return error_reply(b"ERR syntax error");
                }

                xx = true;
                i += 1;
            } else {
                return error_reply(b"ERR syntax error");
            }
        }

        let mut store = match store.lock() {
            Ok(store) => store,
            Err(_) => return error_reply(b"ERR internal server error"),
        };

        let key = &command[1];

        // 만료된 키가 HashMap에 남아 있을 수 있으므로,
        // NX/XX 판단 전에 lazy expiration으로 먼저 제거한다.
        // 그렇지 않으면 만료된 키를 존재하는 것으로 잘못 판단할 수 있다.
        if store.get(key).is_some_and(is_expired) {
            store.remove(key);
        }

        let exists = store.contains_key(key);

        if nx && exists {
            return b"$-1\r\n".to_vec();
        }

        if xx && !exists {
            return b"$-1\r\n".to_vec();
        }

        // TTL이 설정되어 있으면 expires_at을 계산하고, 없으면 None으로 설정합니다.
        let expires_at = ttl.map(|duration| Instant::now() + duration);

        store.insert(
            key.clone(),
            Entry {
                value: command[2].clone(),
                expires_at,
            },
        );

        return b"+OK\r\n".to_vec();
    }

    if name.eq_ignore_ascii_case(b"GET") {
        if command.len() != 2 {
            return error_reply(b"ERR wrong number of arguments for 'get' command");
        }

        let mut store = match store.lock() {
            Ok(store) => store,
            Err(_) => return error_reply(b"ERR internal server error"),
        };

        let key = &command[1];

        if store.get(key).is_some_and(is_expired) {
            store.remove(key);
            return b"$-1\r\n".to_vec();
        }

        return match store.get(key) {
            Some(entry) => bulk_string(&entry.value),
            None => b"$-1\r\n".to_vec(),
        };
    }

    // EXPIRE 명령은 key가 존재하지 않으면 0을, 성공하면 1을 돌려줍니다.
    if name.eq_ignore_ascii_case(b"EXPIRE") {
        if command.len() != 3 {
            return error_reply(b"ERR wrong number of arguments for 'expire' command");
        }

        // seconds 인자를 i64로 파싱합니다. 음수도 허용합니다.
        let seconds = match std::str::from_utf8(&command[2])
            .ok()
            .and_then(|s| s.parse::<i64>().ok())
        {
            Some(seconds) => seconds,
            None => return error_reply(b"ERR value is not an integer or out of range"),
        };

        let mut store = match store.lock() {
            Ok(store) => store,
            Err(_) => return error_reply(b"ERR internal server error"),
        };

        let key = &command[1];

        if store.get(key).is_some_and(is_expired) {
            // 만료된 키는 삭제하고 0을 반환
            store.remove(key);
            return b":0\r\n".to_vec();
        }

        let Some(entry) = store.get_mut(key) else {
            return b":0\r\n".to_vec();
        };

        if seconds <= 0 {
            store.remove(key);
            return b":1\r\n".to_vec();
        }

        // seconds가 양수이면 expires_at을 설정합니다.
        entry.expires_at = Some(Instant::now() + Duration::from_secs(seconds as u64));

        return b":1\r\n".to_vec();
    }

    // TTL 명령은 key가 존재하지 않으면 -2를, 만료되지 않은 키는 -1을, 만료된 키는 0 이상을 돌려줍니다.
    if name.eq_ignore_ascii_case(b"TTL") {
        if command.len() != 2 {
            return error_reply(b"ERR wrong number of arguments for 'ttl' command");
        }

        let mut store = match store.lock() {
            Ok(store) => store,
            Err(_) => return error_reply(b"ERR internal server error"),
        };

        let key = &command[1];
        let now = Instant::now();

        // 만료된 키는 삭제하고 -2를 반환
        if store
            .get(key)
            .is_some_and(|entry| entry.expires_at.is_some_and(|expires_at| now >= expires_at))
        {
            store.remove(key);
            return b":-2\r\n".to_vec();
        }

        // key가 존재하지 않으면 -2를 반환
        let Some(entry) = store.get(key) else {
            return b":-2\r\n".to_vec();
        };

        // expires_at이 없으면 -1을 반환
        let Some(expires_at) = entry.expires_at else {
            return b":-1\r\n".to_vec();
        };

        // 남은 시간을 초 단위로 계산하여 반환
        let remaining = expires_at.duration_since(now);
        let ttl = (remaining.as_millis() + 500) / 1000;

        return format!(":{ttl}\r\n").into_bytes();
    }

    // PERSIST 명령은 key가 존재하지 않으면 0을, 만료되지 않은 키는 0을, 만료된 키는 1을 돌려줍니다.
    if name.eq_ignore_ascii_case(b"PERSIST") {
        if command.len() != 2 {
            return error_reply(b"ERR wrong number of arguments for 'persist' command");
        }

        let mut store = match store.lock() {
            Ok(store) => store,
            Err(_) => return error_reply(b"ERR internal server error"),
        };

        let key = &command[1];

        // 만료된 키는 삭제하고 0을 반환
        if store.get(key).is_some_and(is_expired) {
            store.remove(key);
            return b":0\r\n".to_vec();
        }

        // key가 존재하지 않으면 0을 반환
        let Some(entry) = store.get_mut(key) else {
            return b":0\r\n".to_vec();
        };

        // expires_at이 있으면 제거하고 1을 반환, 없으면 0을 반환
        if entry.expires_at.take().is_some() {
            return b":1\r\n".to_vec();
        }

        return b":0\r\n".to_vec();
    }

    if name.eq_ignore_ascii_case(b"DEL") {
        if command.len() < 2 {
            return error_reply(b"ERR wrong number of arguments for 'del' command");
        }

        let mut store = match store.lock() {
            Ok(store) => store,
            Err(_) => return error_reply(b"ERR internal server error"),
        };

        let mut removed = 0;

        for key in &command[1..] {
            // 만료된 키는 제거하고 건너뜁니다.
            if store.get(key).is_some_and(is_expired) {
                store.remove(key);
                continue;
            }

            if store.remove(key).is_some() {
                removed += 1;
            }
        }

        return format!(":{removed}\r\n").into_bytes();
    }

    if name.eq_ignore_ascii_case(b"EXISTS") {
        if command.len() < 2 {
            return error_reply(b"ERR wrong number of arguments for 'exists' command");
        }

        let mut store = match store.lock() {
            Ok(store) => store,
            Err(_) => return error_reply(b"ERR internal server error"),
        };

        let mut count = 0;

        for key in &command[1..] {
            // 만료된 키는 제거하고 건너뜁니다.
            if store.get(key).is_some_and(is_expired) {
                store.remove(key);
                continue;
            }

            if store.contains_key(key) {
                count += 1;
            }
        }
        return format!(":{count}\r\n").into_bytes();
    }

    unknown_command_reply(command)
}

fn bulk_string(value: &[u8]) -> Vec<u8> {
    let mut reply = format!("${}\r\n", value.len()).into_bytes();
    reply.extend_from_slice(value);
    reply.extend_from_slice(b"\r\n");
    reply
}

fn error_reply(message: &[u8]) -> Vec<u8> {
    let mut reply = vec![b'-'];
    reply.extend_from_slice(message);
    reply.extend_from_slice(b"\r\n");
    reply
}

fn unknown_command_reply(command: &[Vec<u8>]) -> Vec<u8> {
    let Some(name) = command.first() else {
        return Vec::new();
    };

    let mut message = format!("ERR unknown command '{}'", String::from_utf8_lossy(name));
    if command.len() > 1 {
        message.push_str(", with args beginning with: ");
        for argument in &command[1..] {
            // 진짜 Redis는 인자마다 뒤에 공백을 하나씩 붙입니다.
            message.push_str(&format!("'{}' ", String::from_utf8_lossy(argument)));
        }
    }

    error_reply(message.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn test_store() -> Store {
        Arc::new(Mutex::new(HashMap::new()))
    }

    fn complete(buffer: &[u8]) -> (Vec<Vec<u8>>, usize) {
        match parse_command(buffer) {
            Parsed::Complete(command, consumed) => (command, consumed),
            _ => panic!("완성된 명령이어야 합니다"),
        }
    }

    #[test]
    fn resp_배열을_파싱한다() {
        let (command, consumed) = complete(b"*2\r\n$4\r\nPING\r\n$5\r\nhello\r\n");
        assert_eq!(command, vec![b"PING".to_vec(), b"hello".to_vec()]);
        assert_eq!(consumed, 25);
    }

    #[test]
    fn 명령이_쪼개져_들어오면_기다린다() {
        assert!(matches!(
            parse_command(b"*2\r\n$4\r\nPING\r\n$5\r\nhel"),
            Parsed::Incomplete
        ));
    }

    #[test]
    fn 깨진_형식은_broken_이다() {
        assert!(matches!(parse_command(b"*1\r\n+PING\r\n"), Parsed::Broken));
    }

    #[test]
    fn ping_응답이_진짜_redis와_같다() {
        assert_eq!(
            build_reply(&[b"PING".to_vec()], &test_store()),
            b"+PONG\r\n".to_vec()
        );
        assert_eq!(
            build_reply(&[b"ping".to_vec()], &test_store()),
            b"+PONG\r\n".to_vec()
        );
        assert_eq!(
            build_reply(&[b"PING".to_vec(), b"hello".to_vec()], &test_store()),
            b"$5\r\nhello\r\n".to_vec()
        );
        assert_eq!(
            build_reply(
                &[b"PING".to_vec(), b"a".to_vec(), b"b".to_vec()],
                &test_store()
            ),
            b"-ERR wrong number of arguments for 'ping' command\r\n".to_vec()
        );
    }

    #[test]
    fn 모르는_커맨드_문구가_진짜_redis와_같다() {
        assert_eq!(
            build_reply(&[b"garbage".to_vec()], &test_store()),
            b"-ERR unknown command 'garbage'\r\n".to_vec()
        );
        assert_eq!(
            build_reply(
                &[b"foo".to_vec(), b"a".to_vec(), b"b".to_vec()],
                &test_store()
            ),
            b"-ERR unknown command 'foo', with args beginning with: 'a' 'b' \r\n".to_vec()
        );
    }

    #[test]
    fn echo_응답이_진짜_redis와_같다() {
        assert_eq!(
            build_reply(&[b"ECHO".to_vec(), b"hello".to_vec()], &test_store()),
            b"$5\r\nhello\r\n".to_vec()
        );

        assert_eq!(
            build_reply(
                &[b"echo".to_vec(), "한글".as_bytes().to_vec()],
                &test_store()
            ),
            "$6\r\n한글\r\n".as_bytes().to_vec()
        );

        assert_eq!(
            build_reply(&[b"ECHO".to_vec()], &test_store()),
            b"-ERR wrong number of arguments for 'echo' command\r\n".to_vec()
        );

        assert_eq!(
            build_reply(
                &[b"ECHO".to_vec(), b"a".to_vec(), b"b".to_vec()],
                &test_store()
            ),
            b"-ERR wrong number of arguments for 'echo' command\r\n".to_vec()
        );
    }
}
