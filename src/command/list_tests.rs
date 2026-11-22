//! S06 전체 동작을 공개 명령 처리 함수로 검증합니다.
//! 응답은 문자열 표시가 아니라 CRLF를 포함한 RESP 바이트로 비교합니다.

use super::build_reply;
use crate::db::{Entry, Store, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn store() -> Store {
    Arc::new(Mutex::new(HashMap::new()))
}

fn run(store: &Store, args: &[&[u8]]) -> Vec<u8> {
    let command: Vec<Vec<u8>> = args.iter().map(|arg| arg.to_vec()).collect();
    build_reply(&command, store)
}

#[test]
fn 양끝_삽입과_조회는_순서와_바이너리를_보존한다() {
    let store = store();
    assert_eq!(run(&store, &[b"RPUSH", b"k", b"a", b"b"]), b":2\r\n");
    assert_eq!(run(&store, &[b"LPUSH", b"k", b"c", b"d"]), b":4\r\n");
    assert_eq!(run(&store, &[b"rpush", b"k", b"", b"\0\xff"]), b":6\r\n");
    assert_eq!(run(&store, &[b"llen", b"k"]), b":6\r\n");
    assert_eq!(
        run(&store, &[b"lrange", b"k", b"0", b"-1"]),
        b"*6\r\n$1\r\nd\r\n$1\r\nc\r\n$1\r\na\r\n$1\r\nb\r\n$0\r\n\r\n$2\r\n\0\xff\r\n"
    );
}

#[test]
fn pop의_단일값과_count_배열과_null_응답을_구분한다() {
    let store = store();
    run(&store, &[b"RPUSH", b"k", b"a", b"b", b"c", b"d"]);
    assert_eq!(run(&store, &[b"LPOP", b"k"]), b"$1\r\na\r\n");
    assert_eq!(
        run(&store, &[b"RPOP", b"k", b"2"]),
        b"*2\r\n$1\r\nd\r\n$1\r\nc\r\n"
    );
    assert_eq!(run(&store, &[b"LPOP", b"k", b"0"]), b"*0\r\n");
    assert_eq!(run(&store, &[b"LLEN", b"k"]), b":1\r\n");
    assert_eq!(run(&store, &[b"LPOP", b"k", b"1"]), b"*1\r\n$1\r\nb\r\n");
    assert_eq!(run(&store, &[b"EXISTS", b"k"]), b":0\r\n");
    assert_eq!(run(&store, &[b"LPOP", b"k"]), b"$-1\r\n");
    assert_eq!(run(&store, &[b"RPOP", b"k", b"2"]), b"*-1\r\n");
    assert_eq!(run(&store, &[b"LPOP", b"k", b"0"]), b"*-1\r\n");
    assert_eq!(run(&store, &[b"LRANGE", b"k", b"0", b"-1"]), b"*0\r\n");
    assert_eq!(run(&store, &[b"LLEN", b"k"]), b":0\r\n");
}

#[test]
fn 마지막_원소를_pop하면_ttl까지_삭제되고_문자열_키로_재사용할_수_있다() {
    for pop in [b"LPOP", b"RPOP"] {
        let store = store();
        run(&store, &[b"RPUSH", b"k", b"only"]);
        run(&store, &[b"EXPIRE", b"k", b"60"]);
        assert_eq!(run(&store, &[pop, b"k"]), b"$4\r\nonly\r\n");
        assert_eq!(run(&store, &[b"TTL", b"k"]), b":-2\r\n");
        assert_eq!(run(&store, &[b"SET", b"k", b"new", b"NX"]), b"+OK\r\n");
        assert_eq!(run(&store, &[b"GET", b"k"]), b"$3\r\nnew\r\n");
    }
}

#[test]
fn lrange는_음수와_포함되는_끝점과_극단적인_인덱스를_처리한다() {
    let store = store();
    run(&store, &[b"RPUSH", b"k", b"a", b"b", b"c"]);
    let cases: &[(&str, &str, &[u8])] = &[
        ("0", "0", b"*1\r\n$1\r\na\r\n"),
        ("1", "2", b"*2\r\n$1\r\nb\r\n$1\r\nc\r\n"),
        ("-2", "-1", b"*2\r\n$1\r\nb\r\n$1\r\nc\r\n"),
        ("-99", "0", b"*1\r\n$1\r\na\r\n"),
        ("0", "-99", b"*0\r\n"),
        ("2", "1", b"*0\r\n"),
        ("3", "99", b"*0\r\n"),
        ("9223372036854775807", "9223372036854775807", b"*0\r\n"),
        ("0", "-9223372036854775808", b"*0\r\n"),
        (
            "-9223372036854775808",
            "9223372036854775807",
            b"*3\r\n$1\r\na\r\n$1\r\nb\r\n$1\r\nc\r\n",
        ),
    ];
    for (start, stop, expected) in cases {
        assert_eq!(
            run(
                &store,
                &[b"LRANGE", b"k", start.as_bytes(), stop.as_bytes()]
            ),
            *expected
        );
    }
    assert_eq!(run(&store, &[b"LLEN", b"k"]), b":3\r\n");
}

#[test]
fn 잘못된_정수와_count는_조회나_수정_전에_거절한다() {
    let store = store();
    run(&store, &[b"RPUSH", b"k", b"a", b"b"]);
    for value in ["hello", "+1", "01", "-0", "9223372036854775808", ""] {
        assert_eq!(
            run(&store, &[b"LRANGE", b"k", value.as_bytes(), b"-1"]),
            b"-ERR value is not an integer or out of range\r\n"
        );
        assert_eq!(
            run(&store, &[b"LPOP", b"k", value.as_bytes()]),
            b"-ERR value is out of range, must be positive\r\n"
        );
    }
    assert_eq!(
        run(&store, &[b"RPOP", b"missing", b"-1"]),
        b"-ERR value is out of range, must be positive\r\n"
    );
    assert_eq!(run(&store, &[b"LLEN", b"k"]), b":2\r\n");
    assert_eq!(
        run(&store, &[b"RPOP", b"k", b"9223372036854775807"]),
        b"*2\r\n$1\r\nb\r\n$1\r\na\r\n"
    );
}

#[test]
fn 모든_list_명령은_문자열_키에서_wrongtype을_반환한다() {
    let store = store();
    run(&store, &[b"SET", b"k", b"original"]);
    let commands: &[&[&[u8]]] = &[
        &[b"LPUSH", b"k", b"x"],
        &[b"RPUSH", b"k", b"x"],
        &[b"LPOP", b"k"],
        &[b"RPOP", b"k"],
        &[b"LPOP", b"k", b"0"],
        &[b"RPOP", b"k", b"2"],
        &[b"LRANGE", b"k", b"0", b"-1"],
        &[b"LLEN", b"k"],
    ];
    for command in commands {
        assert_eq!(
            run(&store, command),
            b"-WRONGTYPE Operation against a key holding the wrong kind of value\r\n"
        );
    }
    assert_eq!(run(&store, &[b"GET", b"k"]), b"$8\r\noriginal\r\n");
}

#[test]
fn 만료된_키는_모든_list_명령에서_없는_키로_처리한다() {
    let store = store();
    let cases: &[(&[&[u8]], &[u8])] = &[
        (&[b"LPOP", b"k"], b"$-1\r\n"),
        (&[b"RPOP", b"k", b"2"], b"*-1\r\n"),
        (&[b"LLEN", b"k"], b":0\r\n"),
        (&[b"LRANGE", b"k", b"0", b"-1"], b"*0\r\n"),
        (&[b"RPUSH", b"k", b"new"], b":1\r\n"),
    ];
    for (command, expected) in cases {
        // 만료된 문자열도 타입 검사 전에 제거되어야 합니다.
        store.lock().unwrap().insert(
            b"k".to_vec(),
            Entry {
                value: Value::Str(b"old".to_vec()),
                expires_at: Some(Instant::now()),
            },
        );
        assert_eq!(run(&store, command), *expected);
    }
    assert_eq!(run(&store, &[b"TTL", b"k"]), b":-1\r\n");
}

#[test]
fn 부분_pop과_rpush와_조회는_만료_시각을_바꾸지_않는다() {
    let store = store();
    let deadline = Instant::now() + Duration::from_secs(60);
    store.lock().unwrap().insert(
        b"k".to_vec(),
        Entry {
            value: Value::List(VecDeque::from([
                b"a".to_vec(),
                b"b".to_vec(),
                b"c".to_vec(),
            ])),
            expires_at: Some(deadline),
        },
    );
    let commands: &[&[&[u8]]] = &[
        &[b"RPOP", b"k"],
        &[b"LPOP", b"k", b"1"],
        &[b"RPUSH", b"k", b"d"],
        &[b"LLEN", b"k"],
        &[b"LRANGE", b"k", b"0", b"-1"],
        &[b"LPOP", b"k", b"0"],
    ];
    for command in commands {
        run(&store, command);
        let guard = store.lock().unwrap();
        assert_eq!(
            guard.get(b"k".as_slice()).unwrap().expires_at,
            Some(deadline)
        );
    }
}

#[test]
fn list_명령의_인자수_오류_문구를_보존한다() {
    let store = store();
    let cases: &[(&[&[u8]], &[u8])] = &[
        (
            &[b"RPUSH", b"k"],
            b"-ERR wrong number of arguments for 'rpush' command\r\n",
        ),
        (
            &[b"LPOP"],
            b"-ERR wrong number of arguments for 'lpop' command\r\n",
        ),
        (
            &[b"RPOP", b"k", b"1", b"2"],
            b"-ERR wrong number of arguments for 'rpop' command\r\n",
        ),
        (
            &[b"LRANGE", b"k", b"0"],
            b"-ERR wrong number of arguments for 'lrange' command\r\n",
        ),
        (
            &[b"LLEN", b"k", b"extra"],
            b"-ERR wrong number of arguments for 'llen' command\r\n",
        ),
    ];
    for (command, expected) in cases {
        assert_eq!(run(&store, command), *expected);
    }
}

#[test]
fn 동시_pop은_각_원소를_한_번씩만_꺼낸다() {
    let store = store();
    for n in 0..200 {
        run(&store, &[b"RPUSH", b"k", n.to_string().as_bytes()]);
    }
    let mut workers = Vec::new();
    for _ in 0..4 {
        let store = Arc::clone(&store);
        workers.push(std::thread::spawn(move || {
            let mut replies = Vec::new();
            loop {
                let reply = run(&store, &[b"LPOP", b"k"]);
                if reply == b"$-1\r\n" {
                    break;
                }
                replies.push(reply);
            }
            replies
        }));
    }
    let mut seen = HashSet::new();
    for worker in workers {
        for reply in worker.join().unwrap() {
            assert!(seen.insert(reply), "같은 원소를 두 번 꺼내면 안 됩니다");
        }
    }
    assert_eq!(seen.len(), 200);
    assert_eq!(run(&store, &[b"EXISTS", b"k"]), b":0\r\n");
}
