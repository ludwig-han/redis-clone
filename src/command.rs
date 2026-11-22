//! 명령의 인자/옵션을 검증하고 저장소를 변경한 뒤 응답을 반환합니다.
//! 기존 명령의 lock 범위는 S05와 같으며, List 명령도 처리 도중 잠금을 풀지 않습니다.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::db::{Entry, Store, Value, is_expired};
use crate::number::{parse_list_integer, parse_number};
use crate::reply::{bulk_array, bulk_string, error_reply, unknown_command_reply, wrong_type_reply};

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
        // &entry.value는 값을 꺼내 가져오지 않고 읽기 위해 빌립니다.
        // 숫자 연산은 문자열 값에만 허용합니다. 리스트라면 숫자 파싱 전에 WRONGTYPE입니다.
        Some(entry) => match &entry.value {
            Value::Str(bytes) => match parse_number(bytes) {
                Some(value) => value,
                None => return error_reply(b"ERR value is not an integer or out of range"),
            },
            Value::List(_) => return wrong_type_reply(),
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
            entry.value = Value::Str(next.to_string().into_bytes());
        }
        None => {
            store.insert(
                key.to_vec(),
                Entry {
                    value: Value::Str(next.to_string().into_bytes()),
                    expires_at: None,
                },
            );
        }
    }

    format!(":{next}\r\n").into_bytes()
}

/// LPUSH/RPUSH key element [element ...]의 공통 처리입니다.
/// 명령 전체를 한 번 잠그므로 다른 스레드가 여러 원소 삽입의 중간 상태를 볼 수 없습니다.
fn push(command: &[Vec<u8>], store: &Store) -> Vec<u8> {
    // build_reply에서 LPUSH 또는 RPUSH로 확인된 명령만 이 함수에 전달합니다.
    let push_left = command[0].eq_ignore_ascii_case(b"LPUSH");
    // command[0] = 명령 이름, command[1] = key, command[2..] = 추가할 원소들입니다.
    // 키만 있고 원소가 없는 요청은 잘못된 요청이며, 빈 리스트 키도 만들지 않습니다.
    if command.len() < 3 {
        return if push_left {
            error_reply(b"ERR wrong number of arguments for 'lpush' command")
        } else {
            error_reply(b"ERR wrong number of arguments for 'rpush' command")
        };
    }

    let mut store = match store.lock() {
        Ok(store) => store,
        Err(_) => return error_reply(b"ERR internal server error"),
    };
    // 이 store는 잠금을 유지하는 MutexGuard입니다. 함수에서 나갈 때 잠금이 풀립니다.
    let key = &command[1];

    // 만료된 문자열 키도 이미 없는 키입니다. 타입 검사보다 만료 처리를 먼저 합니다.
    if store.get(key).is_some_and(is_expired) {
        store.remove(key);
    }

    // entry는 HashMap의 '이 키의 자리'를 나타내는 API입니다.
    // or_insert_with는 키가 없을 때만 || { ... } 안의 코드를 실행해 새 Entry를 만듭니다.
    // 이미 있으면 그 Entry를 그대로 빌려오므로 기존 리스트의 expires_at도 유지됩니다.
    // HashMap이 키를 소유해야 하므로, command에서 빌린 key를 clone해 전달합니다.
    let entry = store.entry(key.clone()).or_insert_with(|| Entry {
        value: Value::List(VecDeque::new()),
        expires_at: None,
    });

    // &mut는 '수정할 수 있게 빌리기'입니다. 리스트 전체를 복사하거나 꺼내지 않습니다.
    // match가 List임을 확인하면 list는 저장소 안 VecDeque에 대한 &mut 참조가 됩니다.
    let list = match &mut entry.value {
        Value::List(list) => list,
        // _는 안에 든 문자열 바이트가 여기서는 필요 없다는 뜻입니다.
        Value::Str(_) => return wrong_type_reply(),
    };

    for value in &command[2..] {
        // LPUSH a b c는 앞에 넣어 [c, b, a], RPUSH a b c는 뒤에 넣어 [a, b, c]입니다.
        // command는 빌린 요청이고, 저장소는 요청 처리 후에도 원소를 보관해야 합니다.
        // 따라서 각 바이트 묶음을 clone하여 리스트가 소유하게 합니다.
        if push_left {
            list.push_front(value.clone());
        } else {
            list.push_back(value.clone());
        }
    }

    // ':'는 RESP 정수 응답입니다. 성공 여부가 아니라 삽입 후 전체 길이를 돌려줍니다.
    format!(":{}\r\n", list.len()).into_bytes()
}

/// LPOP/RPOP은 원소를 저장소에서 꺼내 응답으로 돌려줍니다.
/// count를 생략하면 bulk string 하나, 지정하면 count가 1이어도 배열을 반환합니다.
fn pop(command: &[Vec<u8>], store: &Store) -> Vec<u8> {
    let pop_left = command[0].eq_ignore_ascii_case(b"LPOP");
    if command.len() != 2 && command.len() != 3 {
        return if pop_left {
            error_reply(b"ERR wrong number of arguments for 'lpop' command")
        } else {
            error_reply(b"ERR wrong number of arguments for 'rpop' command")
        };
    }
    let has_count = command.len() == 3;
    let count = if has_count {
        // count 검증은 키 조회보다 먼저 합니다. 없는 키라도 잘못된 count에는 오류입니다.
        match parse_list_integer(&command[2]) {
            Some(count) if count >= 0 => count,
            _ => return error_reply(b"ERR value is out of range, must be positive"),
        }
    } else {
        1
    };

    let mut store = match store.lock() {
        Ok(store) => store,
        Err(_) => return error_reply(b"ERR internal server error"),
    };
    let key = &command[1];
    if store.get(key).is_some_and(is_expired) {
        store.remove(key);
    }
    let Some(entry) = store.get_mut(key) else {
        // RESP2에서 null 배열(*-1)과 null 문자열($-1)은 다른 응답입니다.
        return if has_count { b"*-1\r\n" } else { b"$-1\r\n" }.to_vec();
    };
    let list = match &mut entry.value {
        Value::List(list) => list,
        Value::Str(_) => return wrong_type_reply(),
    };

    // count만큼 미리 메모리를 할당하지 않습니다. 큰 숫자를 받아도 실제 원소만 꺼냅니다.
    let mut values = Vec::new();
    for _ in 0..count {
        let value = if pop_left {
            list.pop_front()
        } else {
            list.pop_back()
        };
        match value {
            // pop은 Vec<u8>의 소유권을 넘겨주므로 여기서는 clone이 필요 없습니다.
            Some(value) => values.push(value),
            None => break,
        }
    }

    // Redis는 빈 리스트 키를 남기지 않습니다. 원소가 남으면 Entry와 TTL을 유지합니다.
    // 아래에서는 list를 더 쓰지 않아 빌림이 끝나므로, 같은 HashMap에서 키를 지울 수 있습니다.
    if list.is_empty() {
        store.remove(key);
    }

    if has_count {
        let borrowed: Vec<&[u8]> = values.iter().map(|value| value.as_slice()).collect();
        bulk_array(&borrowed)
    } else {
        match values.first() {
            Some(value) => bulk_string(value),
            None => b"$-1\r\n".to_vec(),
        }
    }
}

/// LLEN은 리스트의 길이를 읽습니다. 만료된 키를 지울 수 있어 저장소를 mut로 잠급니다.
fn llen(command: &[Vec<u8>], store: &Store) -> Vec<u8> {
    if command.len() != 2 {
        return error_reply(b"ERR wrong number of arguments for 'llen' command");
    }
    let mut store = match store.lock() {
        Ok(store) => store,
        Err(_) => return error_reply(b"ERR internal server error"),
    };
    let key = &command[1];
    if store.get(key).is_some_and(is_expired) {
        store.remove(key);
    }
    match store.get(key) {
        Some(entry) => match &entry.value {
            Value::List(list) => format!(":{}\r\n", list.len()).into_bytes(),
            Value::Str(_) => wrong_type_reply(),
        },
        None => b":0\r\n".to_vec(),
    }
}

/// LRANGE의 stop은 해당 위치를 포함하는 끝점입니다. -1은 마지막, -2는 그 앞 원소입니다.
fn lrange(command: &[Vec<u8>], store: &Store) -> Vec<u8> {
    if command.len() != 4 {
        return error_reply(b"ERR wrong number of arguments for 'lrange' command");
    }
    let (Some(start), Some(stop)) = (
        parse_list_integer(&command[2]),
        parse_list_integer(&command[3]),
    ) else {
        return error_reply(b"ERR value is not an integer or out of range");
    };
    let mut store = match store.lock() {
        Ok(store) => store,
        Err(_) => return error_reply(b"ERR internal server error"),
    };
    let key = &command[1];
    if store.get(key).is_some_and(is_expired) {
        store.remove(key);
    }
    let Some(entry) = store.get(key) else {
        return bulk_array(&[]);
    };
    let list = match &entry.value {
        Value::List(list) => list,
        Value::Str(_) => return wrong_type_reply(),
    };

    // 음수 인덱스는 길이를 더해 앞에서부터의 위치로 바꿉니다.
    // 예: 길이 3에서 -1은 3 + (-1) = 2입니다.
    // i128은 입력 i64와 길이를 더하는 중간 계산의 오버플로를 피하기 위해 사용합니다.
    let len = list.len() as i128;
    let start = if start < 0 {
        len + i128::from(start)
    } else {
        i128::from(start)
    };
    let stop = if stop < 0 {
        len + i128::from(stop)
    } else {
        i128::from(stop)
    };
    let start = start.max(0);
    let stop = stop.min(len - 1);
    if start > stop {
        return bulk_array(&[]);
    }

    // 위에서 범위를 보정했으므로 여기서는 0 <= start <= stop < len입니다.
    // ..=는 마지막 위치도 포함하는 Rust 범위 문법입니다.
    let values: Vec<&[u8]> = list
        .range(start as usize..=stop as usize)
        .map(|value| value.as_slice())
        .collect();
    bulk_array(&values)
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

    // 연결 함수는 명령 종류를 몰라도 됩니다. 실행할 함수의 선택은 여기서 합니다.
    if name.eq_ignore_ascii_case(b"LPUSH") || name.eq_ignore_ascii_case(b"RPUSH") {
        return push(command, store);
    }
    if name.eq_ignore_ascii_case(b"LPOP") || name.eq_ignore_ascii_case(b"RPOP") {
        return pop(command, store);
    }
    if name.eq_ignore_ascii_case(b"LLEN") {
        return llen(command, store);
    }
    if name.eq_ignore_ascii_case(b"LRANGE") {
        return lrange(command, store);
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
                // SET은 기존 값이 리스트여도 문자열로 교체합니다.
                // 위에서 NX/XX 조건을 먼저 검사했고, TTL도 SET 옵션에 맞춰 새로 정합니다.
                value: Value::Str(command[2].clone()),
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
            // GET은 문자열을 읽는 명령입니다. 리스트를 문자열처럼 직렬화하지 않습니다.
            Some(entry) => match &entry.value {
                Value::Str(bytes) => bulk_string(bytes),
                Value::List(_) => wrong_type_reply(),
            },
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

// S06 전체 명령의 조합/경계값 테스트는 자식 모듈로 나누어 둡니다.
#[cfg(test)]
mod list_tests;

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

    // 저장 내용도 직접 검사해 LPUSH가 실제로 원소의 순서를 바꾸었는지 확인합니다.
    // 실제 명령 실행은 서버와 동일하게 build_reply를 통합니다.
    #[test]
    fn lpush는_여러_원소를_인자_순서대로_맨_앞에_삽입한다() {
        let store = test_store();
        assert_eq!(run(&store, &[b"LPUSH", b"jobs", b"a", b"b"]), b":2\r\n");
        assert_eq!(run(&store, &[b"lpush", b"jobs", b"", b"\0\xff"]), b":4\r\n");

        let guard = store.lock().unwrap();
        let entry = guard.get(b"jobs".as_slice()).unwrap();
        let Value::List(list) = &entry.value else {
            panic!("LPUSH 이후의 값은 리스트여야 합니다");
        };
        let expected = VecDeque::from([
            b"\0\xff".to_vec(),
            b"".to_vec(),
            b"b".to_vec(),
            b"a".to_vec(),
        ]);
        assert_eq!(list, &expected);
        assert_eq!(entry.expires_at, None);
    }

    #[test]
    fn lpush의_인자가_부족하면_키를_만들지_않는다() {
        let store = test_store();
        let expected = b"-ERR wrong number of arguments for 'lpush' command\r\n";
        assert_eq!(run(&store, &[b"LPUSH"]), expected);
        assert_eq!(run(&store, &[b"LPUSH", b"jobs"]), expected);
        assert_eq!(run(&store, &[b"EXISTS", b"jobs"]), b":0\r\n");
    }

    #[test]
    fn 문자열에_lpush를_하면_wrongtype이며_값과_ttl을_보존한다() {
        let store = test_store();
        let deadline = Instant::now() + Duration::from_secs(60);
        store.lock().unwrap().insert(
            b"jobs".to_vec(),
            Entry {
                value: Value::Str(b"old".to_vec()),
                expires_at: Some(deadline),
            },
        );
        assert_eq!(
            run(&store, &[b"LPUSH", b"jobs", b"a", b"b"]),
            b"-WRONGTYPE Operation against a key holding the wrong kind of value\r\n"
        );
        assert_eq!(run(&store, &[b"GET", b"jobs"]), b"$3\r\nold\r\n");
        let guard = store.lock().unwrap();
        assert_eq!(
            guard.get(b"jobs".as_slice()).unwrap().expires_at,
            Some(deadline)
        );
    }

    #[test]
    fn 리스트에_문자열_명령을_사용하면_wrongtype이며_리스트를_보존한다() {
        let store = test_store();
        assert_eq!(run(&store, &[b"LPUSH", b"jobs", b"a"]), b":1\r\n");
        let commands: &[&[&[u8]]] = &[
            &[b"GET", b"jobs"],
            &[b"INCR", b"jobs"],
            &[b"DECR", b"jobs"],
            &[b"INCRBY", b"jobs", b"2"],
        ];
        for command in commands {
            assert_eq!(
                run(&store, command),
                b"-WRONGTYPE Operation against a key holding the wrong kind of value\r\n"
            );
        }
        let guard = store.lock().unwrap();
        let Value::List(list) = &guard.get(b"jobs".as_slice()).unwrap().value else {
            panic!("오류 이후에도 리스트여야 합니다");
        };
        assert_eq!(list, &VecDeque::from([b"a".to_vec()]));
    }

    #[test]
    fn 기존_리스트에_lpush하면_정확한_만료_시각을_유지한다() {
        let store = test_store();
        let deadline = Instant::now() + Duration::from_secs(60);
        store.lock().unwrap().insert(
            b"jobs".to_vec(),
            Entry {
                value: Value::List(VecDeque::from([b"old".to_vec()])),
                expires_at: Some(deadline),
            },
        );
        assert_eq!(run(&store, &[b"LPUSH", b"jobs", b"new"]), b":2\r\n");
        let guard = store.lock().unwrap();
        let entry = guard.get(b"jobs".as_slice()).unwrap();
        assert_eq!(entry.expires_at, Some(deadline));
        let Value::List(list) = &entry.value else {
            panic!("리스트여야 합니다");
        };
        assert_eq!(list, &VecDeque::from([b"new".to_vec(), b"old".to_vec()]));
    }

    #[test]
    fn 만료된_문자열과_리스트는_lpush에서_새_키가_된다() {
        let store = test_store();
        for value in [
            Value::Str(b"old".to_vec()),
            Value::List(VecDeque::from([b"old".to_vec()])),
        ] {
            store.lock().unwrap().insert(
                b"jobs".to_vec(),
                Entry {
                    value,
                    expires_at: Some(Instant::now()),
                },
            );
            assert_eq!(run(&store, &[b"LPUSH", b"jobs", b"new"]), b":1\r\n");
            assert_eq!(run(&store, &[b"TTL", b"jobs"]), b":-1\r\n");
            let guard = store.lock().unwrap();
            let Value::List(list) = &guard.get(b"jobs".as_slice()).unwrap().value else {
                panic!("기존 타입과 무관하게 새 리스트여야 합니다");
            };
            assert_eq!(list, &VecDeque::from([b"new".to_vec()]));
        }
    }

    #[test]
    fn set_nx는_리스트를_유지하고_set_xx는_문자열로_교체한다() {
        let store = test_store();
        assert_eq!(run(&store, &[b"LPUSH", b"jobs", b"a"]), b":1\r\n");
        assert_eq!(run(&store, &[b"EXPIRE", b"jobs", b"60"]), b":1\r\n");
        assert_eq!(run(&store, &[b"SET", b"jobs", b"new", b"NX"]), b"$-1\r\n");
        assert_eq!(run(&store, &[b"LPUSH", b"jobs", b"b"]), b":2\r\n");
        assert_eq!(run(&store, &[b"SET", b"jobs", b"new", b"XX"]), b"+OK\r\n");
        assert_eq!(run(&store, &[b"GET", b"jobs"]), b"$3\r\nnew\r\n");
        assert_eq!(run(&store, &[b"TTL", b"jobs"]), b":-1\r\n");
    }

    #[test]
    fn 동시_lpush에서도_한_명령의_원소들_사이에_다른_명령이_끼지_않는다() {
        let store = test_store();
        let mut workers = Vec::new();
        for _ in 0..4 {
            let store = Arc::clone(&store);
            workers.push(std::thread::spawn(move || {
                for _ in 0..50 {
                    let reply = run(&store, &[b"LPUSH", b"jobs", b"a", b"b"]);
                    assert_eq!(reply.first(), Some(&b':'));
                }
            }));
        }
        for worker in workers {
            worker.join().unwrap();
        }
        let guard = store.lock().unwrap();
        let Value::List(list) = &guard.get(b"jobs".as_slice()).unwrap().value else {
            panic!("리스트여야 합니다");
        };
        assert_eq!(list.len(), 400);
        // 명령마다 [b, a]를 한 묶음으로 넣었으므로 최종 순서도 b, a가 반복되어야 합니다.
        for (index, value) in list.iter().enumerate() {
            let expected = if index % 2 == 0 { b"b" } else { b"a" };
            assert_eq!(value, expected);
        }
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
                value: Value::Str(b"10".to_vec()),
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
        assert!(matches!(&entry.value, Value::Str(bytes) if bytes == b"7"));
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
                    value: Value::Str(b"99".to_vec()),
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
