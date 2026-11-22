# S06 List — 한호택

- 작업 브랜치: `hht/s06-list`
- 시작 커밋: `7f24aed` (S05 리팩터링 완료)
- 구현 범위: LPUSH, RPUSH, LPOP, RPOP, LRANGE, LLEN 및 WRONGTYPE
- LPOP/RPOP의 선택적 count 인자도 처리합니다. BLPOP/BRPOP은 S07 범위입니다.
- 커밋과 Push는 하지 않습니다.

## 1. 최종 구조

| 위치 | 책임 |
| --- | --- |
| `src/db.rs` | Value::Str / Value::List, 공통 Entry와 TTL |
| `src/command.rs`의 `push()` | LPUSH/RPUSH 공통 검증과 삽입. 방향만 분기 |
| 같은 파일의 `pop()` | LPOP/RPOP, 선택적 count, 빈 리스트의 키 삭제 |
| 같은 파일의 `lrange()` | 음수 인덱스 보정, 포함되는 끝점, 범위 밖 처리 |
| 같은 파일의 `llen()` | 리스트 길이와 없는 키의 0 응답 |
| `src/reply.rs` | WRONGTYPE와 bulk string 배열 응답 |
| `src/number.rs` | 새 List 명령의 정수 인자 검증 |
| `src/command/list_tests.rs` | List 전체 조합, 경계값, TTL, 동시 POP 테스트 |
| `tests/cases/06-lpush.txt`, `06-list.txt` | 실제 Redis와 명령 응답 비교 |

연결/RESP 파서와 기존 스레드 + Arc<Mutex<HashMap>> 구조는 그대로입니다. 명령 하나는 처리 전체에 걸쳐 같은 잠금을 유지합니다. 외부 Rust 의존성은 추가하지 않았습니다.

## 2. 설계 선택과 이유

사용자는 Value enum + VecDeque를 선택했습니다. 처음에는 LPUSH부터 설명하며 구현했고, 이후 빠른 진행을 요청하여 남은 다섯 명령을 함께 구현했습니다. 이해 확인 질문에 답하는 것을 진행 조건으로 두지 않습니다.

| 선택 | 이유 | 다른 선택과 비용 | 재검토 조건 |
| --- | --- | --- | --- |
| 단일 HashMap + Value enum | 한 키는 한 타입만 갖고 TTL/DEL/EXISTS를 재사용 | 타입별 Map: 키 중복과 만료 관리가 여러 곳으로 나뉨 | 새로운 타입들의 공통 저장소 API가 실제로 필요해질 때 |
| VecDeque<Vec<u8>> | 양끝 삽입/삭제 지원, 원소는 바이너리 보존 | Vec: 앞쪽 이동 비용. 직접 연결 리스트: 노드 소유권 관리 부담 | 실제 측정에서 메모리/접근 패턴 병목이 확인될 때 |
| 명령 전체 Mutex 잠금 | 다중 원소 삽입/삭제 중간에 다른 명령이 끼지 않음 | 원소별 잠금 해제: 중간 상태를 다른 연결이 볼 수 있음 | S07의 대기 처리 또는 측정된 잠금 병목 |

`VecDeque`는 표준 라이브러리의 링 버퍼 기반 양방향 큐입니다. [Rust 공식 문서](https://doc.rust-lang.org/std/collections/struct.VecDeque.html)

## 3. 동작 규칙

- LPUSH는 인자를 순서대로 앞에 넣으므로 a b c를 넣으면 c b a가 됩니다. RPUSH는 뒤에 넣어 a b c가 됩니다.
- LPOP/RPOP은 앞/뒤 원소를 꺼내면서 삭제합니다. 마지막 원소를 꺼내면 키와 TTL도 함께 사라집니다.
- POP에 count가 없으면 bulk string, 있으면 count가 1이어도 배열입니다. count 0은 기존 리스트를 변경하지 않습니다.
- 없는 키의 POP은 count 유무에 따라 null bulk string 또는 null array를 반환합니다. LRANGE는 빈 배열, LLEN은 정수 0입니다.
- LRANGE의 stop은 해당 위치를 포함합니다. -1은 마지막 원소이며 범위를 벗어난 인덱스는 보정하거나 빈 배열로 처리합니다.
- 모든 List 명령은 만료된 키를 타입 검사 전에 제거합니다. 값이 남아 있는 동안 PUSH/POP은 기존 TTL을 유지합니다.
- GET/INCR 계열을 리스트에 사용하거나 List 명령을 문자열에 사용하면 WRONGTYPE입니다. SET은 조건을 만족하면 기존 리스트를 문자열로 교체합니다.

공식 동작 기준: [LPUSH](https://redis.io/docs/latest/commands/lpush/), [RPUSH](https://redis.io/docs/latest/commands/rpush/), [LPOP](https://redis.io/docs/latest/commands/lpop/), [LRANGE](https://redis.io/docs/latest/commands/lrange/), [LLEN](https://redis.io/docs/latest/commands/llen/).

## 4. 발표에서 설명할 핵심 세 곳

**① Value와 빌림 — 값을 구분해서 저장소 안에서 수정한다**

```rust
pub(super) enum Value {
    Str(Vec<u8>),
    List(VecDeque<Vec<u8>>),
}
```

발표 문장: “enum은 문자열/리스트라는 종류와 실제 데이터를 함께 담습니다. 리스트 안의 Vec<u8> 하나가 원소 하나이고, TTL은 원소가 아닌 키 전체의 정보라 Entry에 그대로 두었습니다.”

`match &mut entry.value`는 값의 종류를 검사하면서 수정할 수 있게 빌립니다. 리스트 전체를 clone하지 않습니다. PUSH는 빌린 요청 바이트를 저장소가 소유해야 하므로 원소를 clone합니다. POP은 원소의 소유권을 저장소에서 꺼내 받기 때문에 clone이 필요 없습니다.

**② pop()의 마지막 원소 처리 — 빈 리스트 키를 남기지 않는다**

```rust
if list.is_empty() {
    store.remove(key);
}
```

발표 문장: “원소를 다 꺼내면 Redis 규칙에 따라 키 자체를 삭제합니다. 원소가 남아 있으면 Entry를 그대로 두므로 TTL이 유지됩니다. list 참조를 더 쓰지 않는 지점에서 빌림이 끝나기 때문에, 이후 HashMap에서 키를 제거할 수 있습니다.”

count가 있을 때는 꺼낸 원소들의 배열을 만듭니다. RPOP 여러 개는 오른쪽에서 꺼낸 순서로 응답합니다. 사용자가 준 큰 count만큼 메모리를 미리 할당하지 않고 실제 원소만 꺼내며, 리스트가 비면 반복을 끝냅니다.

**③ lrange()의 범위 보정과 배열 응답 — 음수를 먼저 처리한다**

발표 문장: “음수 인덱스에는 리스트 길이를 더합니다. 길이가 3이면 -1은 2가 됩니다. 시작을 0 이상, 끝을 마지막 인덱스 이하로 보정하고, 시작이 끝보다 크면 빈 배열을 돌려줍니다.”

중간 계산은 i128로 수행해 i64의 극단적인 입력에서도 덧셈이 넘치지 않게 합니다. 음수를 바로 usize로 바꾸지 않습니다. 범위를 확인한 뒤 `start..=stop`으로 조회하며 `..=`는 끝점을 포함합니다.

`bulk_array(&[&[u8]])`에는 원소의 바이트를 빌려 전달합니다. 배열은 `*개수\r\n` 뒤에 각 bulk string을 이어 붙여 만듭니다. 빈 배열 `*0\r\n`, null 배열 `*-1\r\n`, null bulk string `$-1\r\n`은 서로 구분합니다.

## 5. 새 Rust 개념 — 3줄

- enum/match는 값의 종류와 데이터를 안전하게 구분합니다.
- &mut는 수정 가능한 빌림이며, POP이 돌려주는 Vec<u8>는 소유권을 넘겨받은 값입니다.
- `..=`는 양끝을 포함하는 범위이고, `Vec<&[u8]>`는 바이트 자체가 아닌 참조를 모은 목록입니다.

## 6. 최종 검증 결과

2026-11-22 사용자 WSL 실행 로그를 확인했습니다. Rust 1.98.1 / Redis 서버·CLI 8.10.2에서 아래 검증이 모두 통과했습니다. 현재 소스는 테스트한 복사본과 바이트 단위로 일치합니다.

| 검증 | 최종 결과 |
| --- | --- |
| cargo fmt --check | 통과 |
| cargo clippy -- -D warnings | 통과 |
| cargo clippy --all-targets -- -D warnings | 통과 |
| cargo test | 31개 통과 / 0개 실패 |
| ./scripts/difftest.sh | 224개 통과 / 0개 실패 |
| 원시 RESP 바이트 비교 | 254개 통과 / 0개 실패 |

S06의 여섯 명령 전체 구현과 검증을 완료했습니다. 이하에는 이전 단계 기록과 재현 방법을 남깁니다.

LPUSH 단계에서는 사용자 WSL의 Rust 1.98.1 / Redis 8.10.2에서 단위 테스트 21개와 difftest 112개가 통과했습니다. 이것은 LPUSH 단계의 기록이며 전체 S06 검증 결과와 구분합니다.

전체 S06은 `verify-s06.sh`로 다음을 실행합니다.

```bash
cargo fmt
cargo fmt --check
cargo clippy -- -D warnings
cargo clippy --all-targets -- -D warnings
cargo test
./scripts/difftest.sh
python3 verify-s06-wire.py <빌드한 서버 경로>
```

전체 검증은 기존 21개에 List 종합 테스트 10개, 기존 비교 112개에 List 종합 케이스 112개를 추가합니다. 별도 원시 RESP 검증은 격리된 Redis와 clone을 실행해 바이너리 원소, null 종류, 배열, 파이프라이닝, 극단적 인덱스와 정수 오류의 실제 응답 바이트를 비교합니다.

최종 실행 결과는 작업 산출물의 `s06-full-validation.log`, 환경은 `s06-full-environment.log`에 남깁니다. 스크립트는 원본 작업 트리에 cargo fmt를 적용하고, 임시 복사본에서만 기존 shell/케이스의 CRLF를 LF로 바꿉니다. difftest 포트는 16379/16380, 원시 RESP 비교 포트는 16381/16382이며 기존 Redis를 재사용하지 않습니다.

이 채팅의 샌드박스 계정은 WSL 접근이 거부되므로 실행은 사용자 WSL에서 수행하고 저장된 로그로 확인합니다.

## 7. 이번에 바꾸지 않은 부분

블로킹, Hash, 트랜잭션, 영속성 등 S07 이후 기능은 추가하지 않았습니다. 기존 파서의 잘못된 CRLF 수용 등 S05에서 보고한 문제도 별도 작업으로 남겨 둡니다. 새로운 List 정수 인자에는 Redis 형식 검사를 적용하되 기존 S01~S05의 정수 파서를 변경하지 않았습니다.

현재 변경은 커밋하지 않은 작업 트리에 남기며, 커밋과 Push는 사용자가 요청할 때 진행합니다.
