//! 명령의 인자/옵션을 검증하고 저장소를 변경한 뒤 응답을 반환합니다.
//! 각 명령의 lock 범위는 S05와 같으며, 읽기와 쓰기 사이에 잠금을 풀지 않습니다.

use std::time::{Duration, Instant};

use crate::db::{Entry, Store, is_expired};
use crate::number::parse_number;
use crate::reply::{bulk_string, error_reply, unknown_command_reply};

fn increment_by(store: &Store, key: &[u8], amount: i64) -> Vec<u8> {
    let mut store = match store.lock() {
        Ok(store) => store,
        Err(_) => return error_reply(b"ERR internal server error"),
    };

    // 만료된 키는 없는 키로 취급합니다.
    if store.get(key).is_some_and(is_expired) {
        store.remove(key);
    }

    // 기존 값이 있으면 숫자로 변환하고,
    // 키가 없으면 Redis 규칙에 따라 0부터 시작합니다.
    let current = match store.get(key) {
        Some(entry) => match parse_number(&entry.value) {
            Some(value) => value,
            None => return error_reply(b"ERR value is not an integer or out of range"),
        },
        None => 0,
    };

    // amount를 더하되 i64 overflow를 검사합니다.
    let Some(next) = current.checked_add(amount) else {
        return error_reply(b"ERR increment or decrement would overflow");
    };

    // 기존 키라면 value만 바꿔 TTL을 유지합니다.
    match store.get_mut(key) {
        Some(entry) => {
            entry.value = next.to_string().into_bytes();
        }
        None => {
            store.insert(
                key.to_vec(),
                Entry {
                    value: next.to_string().into_bytes(),
                    expires_at: None,
                },
            );
        }
    }

    format!(":{next}\r\n").into_bytes()
}

// 명령어에 따라 적절한 응답을 만듭니다.
pub(super) fn build_reply(command: &[Vec<u8>], store: &Store) -> Vec<u8> {
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

    if name.eq_ignore_ascii_case(b"INCR") {
        if command.len() != 2 {
            return error_reply(b"ERR wrong number of arguments for 'incr' command");
        }

        return increment_by(store, &command[1], 1);
    }

    if name.eq_ignore_ascii_case(b"DECR") {
        if command.len() != 2 {
            return error_reply(b"ERR wrong number of arguments for 'decr' command");
        }

        return increment_by(store, &command[1], -1);
    }

    if name.eq_ignore_ascii_case(b"INCRBY") {
        if command.len() != 3 {
            return error_reply(b"ERR wrong number of arguments for 'incrby' command");
        }

        let amount = match parse_number(&command[2]) {
            Some(value) => value,
            None => return error_reply(b"ERR value is not an integer or out of range"),
        };

        return increment_by(store, &command[1], amount);
    }

    unknown_command_reply(command)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};

    fn test_store() -> Store {
        Arc::new(Mutex::new(HashMap::new()))
    }

    fn run(store: &Store, args: &[&[u8]]) -> Vec<u8> {
        let command: Vec<Vec<u8>> = args.iter().map(|arg| arg.to_vec()).collect();
        build_reply(&command, store)
    }

    #[test]
    fn 저장과_조회는_바이너리를_보존하고_중복_키를_기존대로_센다() {
        let store = test_store();
        assert_eq!(run(&store, &[b"SET", b"key", b"a\0b"]), b"+OK\r\n");
        assert_eq!(run(&store, &[b"GET", b"key"]), b"$3\r\na\0b\r\n");
        assert_eq!(run(&store, &[b"EXISTS", b"key", b"key"]), b":2\r\n");
        assert_eq!(run(&store, &[b"DEL", b"key", b"key"]), b":1\r\n");
        assert_eq!(run(&store, &[b"GET", b"key"]), b"$-1\r\n");
        assert!(run(&store, &[]).is_empty());
    }

    #[test]
    fn 숫자_연산은_ttl을_유지하고_오류는_값을_바꾸지_않는다() {
        let store = test_store();
        let deadline = Instant::now() + Duration::from_secs(60);
        store.lock().unwrap().insert(
            b"key".to_vec(),
            Entry {
                value: b"10".to_vec(),
                expires_at: Some(deadline),
            },
        );
        assert_eq!(run(&store, &[b"INCR", b"key"]), b":11\r\n");
        assert_eq!(run(&store, &[b"DECR", b"key"]), b":10\r\n");
        assert_eq!(run(&store, &[b"INCRBY", b"key", b"-3"]), b":7\r\n");
        assert_eq!(
            run(&store, &[b"INCRBY", b"key", b"hello"]),
            b"-ERR value is not an integer or out of range\r\n"
        );
        let guard = store.lock().unwrap();
        let entry = guard.get(b"key".as_slice()).unwrap();
        assert_eq!(entry.value, b"7");
        assert_eq!(entry.expires_at, Some(deadline));
    }

    #[test]
    fn 오버플로와_숫자가_아닌_값의_오류_바이트를_보존한다() {
        let store = test_store();
        for (value, command, expected) in [
            (
                b"9223372036854775807".as_slice(),
                b"INCR".as_slice(),
                b"-ERR increment or decrement would overflow\r\n".as_slice(),
            ),
            (
                b"-9223372036854775808".as_slice(),
                b"DECR".as_slice(),
                b"-ERR increment or decrement would overflow\r\n".as_slice(),
            ),
            (
                b"hello".as_slice(),
                b"INCR".as_slice(),
                b"-ERR value is not an integer or out of range\r\n".as_slice(),
            ),
        ] {
            assert_eq!(run(&store, &[b"SET", b"key", value]), b"+OK\r\n");
            assert_eq!(run(&store, &[command, b"key"]), expected);
            assert_eq!(run(&store, &[b"GET", b"key"]), bulk_string(value));
        }
    }

    #[test]
    fn 만료된_키는_명령마다_없는_키로_처리한다() {
        let store = test_store();
        let cases: &[(&[&[u8]], &[u8])] = &[
            (&[b"GET", b"key"], b"$-1\r\n"),
            (&[b"DEL", b"key"], b":0\r\n"),
            (&[b"EXISTS", b"key"], b":0\r\n"),
            (&[b"EXPIRE", b"key", b"10"], b":0\r\n"),
            (&[b"TTL", b"key"], b":-2\r\n"),
            (&[b"PERSIST", b"key"], b":0\r\n"),
            (&[b"SET", b"key", b"new", b"XX"], b"$-1\r\n"),
            (&[b"SET", b"key", b"new", b"NX"], b"+OK\r\n"),
            (&[b"INCR", b"key"], b":1\r\n"),
        ];
        for (command, expected) in cases {
            store.lock().unwrap().insert(
                b"key".to_vec(),
                Entry {
                    value: b"99".to_vec(),
                    expires_at: Some(Instant::now()),
                },
            );
            assert_eq!(run(&store, command), *expected);
        }
        assert_eq!(run(&store, &[b"TTL", b"key"]), b":-1\r\n");
    }

    #[test]
    fn set_옵션과_expire_persist의_응답을_보존한다() {
        let store = test_store();
        assert_eq!(
            run(&store, &[b"SET", b"key", b"old", b"EX", b"60"]),
            b"+OK\r\n"
        );
        let deadline = store
            .lock()
            .unwrap()
            .get(b"key".as_slice())
            .unwrap()
            .expires_at;
        assert_eq!(run(&store, &[b"SET", b"key", b"new", b"NX"]), b"$-1\r\n");
        assert_eq!(
            store
                .lock()
                .unwrap()
                .get(b"key".as_slice())
                .unwrap()
                .expires_at,
            deadline
        );
        assert_eq!(run(&store, &[b"GET", b"key"]), b"$3\r\nold\r\n");
        assert_eq!(run(&store, &[b"PERSIST", b"key"]), b":1\r\n");
        assert_eq!(run(&store, &[b"PERSIST", b"key"]), b":0\r\n");
        assert_eq!(run(&store, &[b"TTL", b"key"]), b":-1\r\n");
        assert_eq!(run(&store, &[b"EXPIRE", b"key", b"0"]), b":1\r\n");
        assert_eq!(run(&store, &[b"TTL", b"key"]), b":-2\r\n");
        assert_eq!(
            run(&store, &[b"SET", b"key", b"old", b"PX", b"60000"]),
            b"+OK\r\n"
        );
        assert_eq!(run(&store, &[b"SET", b"key", b"new", b"XX"]), b"+OK\r\n");
        assert_eq!(run(&store, &[b"TTL", b"key"]), b":-1\r\n");
        assert_eq!(
            run(&store, &[b"SET", b"key", b"new", b"NX", b"XX"]),
            b"-ERR syntax error\r\n"
        );
        assert_eq!(
            run(&store, &[b"SET", b"key", b"new", b"EX", b"0"]),
            b"-ERR invalid expire time in 'set' command\r\n"
        );
    }

    #[test]
    fn 여러_스레드의_incr는_갱신을_잃지_않는다() {
        let store = test_store();
        let mut workers = Vec::new();
        for _ in 0..4 {
            let store = Arc::clone(&store);
            workers.push(std::thread::spawn(move || {
                for _ in 0..100 {
                    let reply = run(&store, &[b"INCR", b"counter"]);
                    assert_eq!(reply.first(), Some(&b':'));
                }
            }));
        }
        for worker in workers {
            worker.join().unwrap();
        }
        assert_eq!(run(&store, &[b"GET", b"counter"]), b"$3\r\n400\r\n");
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
