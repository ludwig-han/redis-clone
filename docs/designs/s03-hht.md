# S03 SET / GET / DEL / EXISTS — 한호택

- 구현 브랜치: `hht/s03-set-get`
- 걸린 시간: 약 ?시간

## 1. 무엇을 만들었나

메모리 안에 key-value 데이터를 저장할 수 있도록 공용 저장소를 추가하고 다음 명령을 구현했습니다.

- `SET key value`
- `GET key`
- `DEL key [key ...]`
- `EXISTS key [key ...]`

없는 키를 `GET`하면 Redis와 동일하게 Null Bulk String(`$-1\r\n`)을 반환합니다.

## 2. 설계 선택 — 이 문서의 핵심

| 갈림길 | 고른 것 | 이유 | 버린 선택지와 그 이유 |
| --- | --- | --- | --- |
| 동시성 모델 | `Arc<Mutex<HashMap<...>>>` | 현재 서버가 connection마다 thread를 생성하므로 여러 thread가 하나의 저장소를 공유해야 했음. 구현이 비교적 단순하고 현재 규모에서는 충분하다고 판단 | 키 해시 샤딩: 지금 단계에서는 구조가 복잡해짐 / actor + channel: 현재 필요한 기능보다 구현 부담이 크다고 판단 |
| 값 타입 | `Vec<u8>` | 기존 RESP parser가 데이터를 바이트 단위로 다루고 있고 Redis 값이 반드시 UTF-8 문자열일 필요는 없기 때문 | `String`: 사용하기는 쉽지만 모든 값을 UTF-8 문자열로 제한하게 됨 |

**나중에 이 선택을 뒤집어야 하는 조건:**

여러 클라이언트가 동시에 접근하면서 하나의 `Mutex`가 병목이 되거나, 이후 블로킹 명령을 구현하면서 락 구조가 문제가 되면 동시성 모델을 다시 검토합니다.

## 3. AI를 어떻게 썼나

- **첫 프롬프트에 뭐라고 썼나:**  
  S02에서 만든 서버를 이어서 S03의 `SET / GET / DEL / EXISTS`를 빠르게 구현하고 싶다고 요청했습니다.

- **AI 제안 중 거절한 것과 이유:**  
  이번에는 구현을 빠르게 끝내는 것을 우선해서 `Arc`, `Mutex`, `lock()` 등의 개념 설명은 나중에 다시 공부하기로 하고 구현부터 진행했습니다.

- **AI가 틀렸던 것 / 스펙과 달랐던 것:**  
  특별히 발견한 것은 없었습니다.

- **이해하지 못한 채 넘어간 코드:**  
  `Arc<Mutex<HashMap<...>>>`, `Arc::clone`, `Mutex::lock()`의 Rust 소유권/동시성 의미는 아직 충분히 이해하지 못했습니다.

- **AI 없이 내가 결정한 것:**  
  S02에서 내가 구현한 코드를 계속 이어서 S03을 구현하기로 했고, 스터디 공식 main의 다른 사람 구현으로 갈아타지 않고 개인 fork에서 내 구현 흐름을 유지하기로 했습니다.

## 4. 내가 추가한 테스트 케이스

`tests/cases/03-set-get.txt`

```text
SET name hotaek
GET name
EXISTS name
DEL name
GET name
EXISTS name

SET a 1
SET b 2
EXISTS a b c
DEL a b c

GET missing
SET 한글 값
GET 한글
```

- 왜 이 케이스를 넣었나:
  기본적인 SET/GET뿐 아니라 삭제 후 조회, 존재하지 않는 키, 여러 키를 대상으로 한 DEL/EXISTS, UTF-8 문자열까지 확인하기 위해 넣었습니다.
- 넣었다가 깨진 것:
  특별히 없었습니다.

## 5. 막혔던 곳
build_reply()가 기존에는 command만 받았지만, S03부터 저장소에 접근해야 해서 Store도 함께 받도록 변경했습니다.

이 때문에 기존 단위 테스트에서 build_reply() 호출 인자가 하나 부족하다는 컴파일 오류가 발생했습니다.

테스트용 Store를 만드는 함수를 추가하고 기존 테스트에서 함께 넘기도록 수정했습니다.

```rust
fn test_store() -> Store {
    Arc::new(Mutex::new(HashMap::new()))
}
```

또한 여러 connection thread가 각각 별도의 HashMap을 가지면 한 클라이언트가 SET한 값을 다른 클라이언트가 볼 수 없기 때문에 하나의 Store를 공유하도록 했습니다.

## 6. 새로 배운 Rust 개념

- Arc를 사용하면 여러 thread가 하나의 데이터를 공유해서 가리킬 수 있습니다.
- Mutex는 공유 데이터에 동시에 접근하는 것을 막기 위해 lock을 사용합니다.
- `Vec<u8>`를 key와 value로 사용하면 문자열뿐 아니라 바이트 데이터를 그대로 저장할 수 있습니다.

※ Arc, Mutex, Arc::clone, lock(), mut의 정확한 의미는 다시 공부할 예정입니다.

## 7. 검증 결과

```sh
$ cargo fmt
```

통과

```sh
$ cargo clippy -- -D warnings
```

통과

```sh
$ cargo test
```

통과

```sh
=== 01-ping.txt ===
  ✅ PING
  ✅ PING
  ✅ PING hello
  ✅ ping
  ✅ PiNg hello
  ✅ PING a b
  ✅ PING a b c
  ✅ garbage
  ✅ NOPE a b
  ✅ PING 한글

=== 02-echo.txt ===
  ✅ ECHO hello
  ✅ echo hello
  ✅ ECHO 한글
  ✅ ECHO
  ✅ ECHO a b

=== 03-set-get.txt ===
  ✅ SET name hotaek
  ✅ GET name
  ✅ EXISTS name
  ✅ DEL name
  ✅ GET name
  ✅ EXISTS name
  ✅ SET a 1
  ✅ SET b 2
  ✅ EXISTS a b c
  ✅ DEL a b c
  ✅ GET missing
  ✅ SET 한글 값
  ✅ GET 한글

------------------------------
통과 28 / 실패 0
```

## 8. 다른 사람 문서를 읽고 (세션 직전에 채움)

- 나와 제일 다른 지점:
- 물어볼 것:
- 남의 케이스로 내 서버를 돌려본 결과: