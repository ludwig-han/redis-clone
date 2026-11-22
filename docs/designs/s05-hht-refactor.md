Rust Redis Clone — S05 이후, S06 이전 리팩터링

작업 위치는 `C:\Users\mcc91\Desktop\redis-clone`입니다. 기존 `hht/refactor-before-s06` 브랜치가 `hht/s05-incr`과 같은 `eb483e4d672fa358e0066cd2c35ba06fba36e4fa` 커밋을 가리키고 있었으므로 그대로 사용했습니다. 시작 시 작업 트리는 깨끗했습니다. 원본 저장소의 main이나 S06 이후 코드는 가져오지 않았고, 커밋·Push·PR은 만들지 않았습니다.

**원래 코드에서 확인한 구조**

`main.rs` 716줄에 서버 시작부터 테스트까지 모여 있었습니다. `TcpListener::incoming()`으로 연결을 받고, `Arc::clone(&store)`한 손잡이를 새 스레드로 넘깁니다. 연결마다 `handle_connection()`이 1,024바이트씩 읽어 버퍼에 누적하고, 완성된 RESP 명령을 모두 처리한 뒤 다음 읽기를 기다립니다. 따라서 TCP 분할 수신과 파이프라이닝은 연결 함수의 책임입니다.

`parse_command()`는 배열 형식의 RESP를 읽어 `Parsed::Complete(command, consumed)`, `Incomplete`, `Broken` 중 하나를 반환합니다. 인라인 명령은 지원하지 않으며, 깨진 입력에는 연결을 닫는 기존 동작을 유지했습니다.

`build_reply()`는 대소문자를 무시해 명령을 선택하고 인자 수와 옵션을 검증합니다. 저장소는 `Arc<Mutex<HashMap<Vec<u8>, Entry>>>`, 값과 키는 바이너리를 보존하는 `Vec<u8>`, 만료 시각은 `Option<Instant>`입니다. 별도 청소 스레드 없이 접근 시 만료된 키를 지웁니다. INCR 계열은 `increment_by()` 안에서 잠금 하나로 읽기·정수 변환·checked_add·쓰기를 완료하며 기존 엔트리의 value만 바꾸므로 TTL이 유지됩니다.

원래 단위 테스트 6개는 파서 3개와 PING/ECHO/알 수 없는 명령 응답 3개였습니다. `tests/cases/01-ping.txt`부터 `05-incr.txt`까지는 difftest로 실행합니다. 서버를 수정할 때 네트워크, 저장소, 프로토콜 코드를 같은 파일에서 찾아야 하고, 파서 테스트까지 명령 구현 아래에 묻혀 있는 점이 가독성을 떨어뜨렸습니다.

**최종 파일 구조**

| 파일 | 책임과 주요 항목 |
| --- | --- |
| `src/main.rs` | 64줄. `mod` 선언, `port_from_args()`, 포트 바인딩, 공유 저장소 생성, 연결 수락 함수 호출 |
| `src/connection.rs` | `accept_connections()`에서 연결별 스레드 생성. `handle_connection()`에서 수신 버퍼, 파이프라인, 연결 종료, 응답 전송 |
| `src/resp.rs` | `Parsed`, `parse_command()`, 비공개 `parse_array()`·`read_line()`, 파서 테스트 4개 |
| `src/command.rs` | `build_reply()`의 명령 선택·인자/옵션 검사·저장소 조작, 비공개 `increment_by()`, 명령 테스트 9개 |
| `src/db.rs` | `Store`, `Entry`, `is_expired()`. 공유 저장소 형태와 만료 여부 판단 |
| `src/reply.rs` | `bulk_string()`, `error_reply()`, `unknown_command_reply()`. 공통 응답 바이트 조립 |
| `src/number.rs` | 프로토콜 길이와 명령 숫자에 공통으로 쓰는 기존 `parse_number()` |

`number.rs`는 RESP 길이 파싱을 위해 command 모듈에 의존하거나, 숫자 연산을 위해 RESP 파서에 의존하는 관계를 피하려고 추가한 5줄짜리 모듈입니다. 기존 변환 함수는 그대로 한 곳에 유지했습니다.

`db.rs`가 모든 저장소 명령을 메서드로 제공하는 구조는 아닙니다. 명령의 TTL 설정·제거와 저장소 잠금은 `command.rs`에 그대로 있습니다. `GET`과 `SET`을 각각 잠그는 메서드로 분리하면 INCR의 읽기와 쓰기 사이에 다른 스레드가 끼어들 수 있습니다. 이번에는 명령 전체의 기존 잠금 범위를 눈으로 확인할 수 있도록 유지했습니다.

`reply.rs`는 공통 조립 함수만 분리했습니다. `+OK`, null bulk string, 정수 응답처럼 명령 안에서 바로 만드는 기존 바이트와 포맷 문자열은 이동시키거나 새 타입으로 감싸지 않았습니다. 파일별 책임은 구분하되 억지로 모든 의존 관계를 계층화하지 않았습니다.

**INCR 요청 하나를 따라가기**

1. `main()`이 포트와 저장소를 준비하고 `accept_connections(listener, store)`를 호출합니다.
2. `accept_connections()`가 연결을 수락하고 공유 저장소의 Arc를 복제해 새 스레드에 전달합니다. HashMap 자체를 복사하지 않습니다.
3. `handle_connection()`이 TCP 바이트를 버퍼에 쌓고 `resp::parse_command(&buffer)`를 호출합니다.
4. `parse_command()` → `parse_array()` → `read_line()`/`number::parse_number()`를 거쳐 명령 인자와 소비한 바이트 수를 얻습니다. 덜 도착했으면 더 읽고, 완성됐으면 해당 바이트만 버퍼에서 제거합니다.
5. `command::build_reply(&command, &store)`가 `INCR`과 인자 수를 확인해 `increment_by(store, key, 1)`을 호출합니다.
6. `increment_by()`가 Mutex를 잠그고 만료된 키를 제거합니다. 기존 값 또는 0을 읽어 정수 변환과 오버플로 검사를 한 뒤 value를 변경합니다. 기존 만료 시각은 유지합니다. 오류가 나면 기존 오류 바이트를 반환합니다.
7. 정수 응답 `:<숫자>\r\n`이 연결 함수로 돌아가고 `write_all()`로 전송됩니다. 버퍼에 다음 명령이 있으면 같은 처리를 반복합니다.

잠금은 기존처럼 명령 처리 함수의 범위가 끝날 때 풀립니다. 소켓 쓰기를 하는 동안 잠금을 계속 보유하도록 바꾸지 않았습니다. EXPIRE/TTL/PERSIST, SET의 EX/PX/NX/XX 판단 순서와 `Instant::now()` 호출 위치도 그대로입니다.

**Rust 모듈을 읽는 방법 — 새 개념 3줄**

- `mod resp;`는 `main.rs` 아래에 resp 모듈을 선언하고 `src/resp.rs`를 그 내용으로 읽게 합니다.
- `use crate::resp::{Parsed, parse_command};`는 이미 선언한 항목을 짧은 이름으로 사용할 수 있게 합니다. `crate`는 현재 바이너리 크레이트의 루트입니다.
- 기본 항목은 비공개이며, `pub(super)`는 부모 모듈 범위에 공개합니다. 현재 형제 모듈들이 공유해야 하는 항목에만 사용하고 내부 도우미는 비공개로 유지했습니다.

`mod`와 `use`는 역할이 다릅니다. 파일을 디렉터리에 추가하기만 해서는 빌드 대상 모듈이 되지 않습니다. 반대로 `use`는 객체를 생성하거나 의존성을 주입하지 않습니다. `#[cfg(test)] mod tests`는 테스트 빌드에만 포함되고, 자식 테스트 모듈에서 `use super::*`로 부모의 비공개 함수까지 검사할 수 있습니다.

**Java/Spring Boot와 연결해서 보기**

| 익숙한 개념 | 이 코드에서 비교할 지점 | 차이 |
| --- | --- | --- |
| 애플리케이션 시작점 | `main.rs` | 객체 생성과 전달을 직접 작성하며 Spring 컨테이너가 없음 |
| 요청 수신·전달 계층 | `connection.rs` | HTTP 컨트롤러가 아니라 TCP 스트림과 버퍼를 직접 처리 |
| 서비스의 명령 처리 | `command.rs` | 클래스/인터페이스 없이 함수가 `&Store`를 받아 실행 |
| 저장소·모델 | `db.rs` | JPA Repository나 외부 DB가 아니라 메모리 HashMap과 엔트리 정의 |
| 직렬화 | `resp.rs`, `reply.rs` | 요청 해석과 응답 조립을 바이트 단위로 직접 구현 |

공통점은 변경 이유에 따라 코드를 찾을 위치를 정한다는 것입니다. 차이점은 Rust 모듈이 컴파일 시의 이름 공간과 공개 범위라는 점입니다. 파일마다 struct와 trait를 만들 필요가 없고, `use`도 Spring의 DI가 아닙니다. Java의 synchronized 구역에 비유할 수 있는 Mutex 잠금은 Rust에서는 guard가 범위를 벗어나면 해제되며, 공유 포인터 Arc와 별개 역할입니다.

**선택 이유와 다시 검토할 조건**

기존 함수를 책임별 파일로 이동하는 방식을 선택했습니다. 지금은 읽는 순서와 테스트 위치를 개선하면서 동작 차이를 작게 만들 수 있습니다. 반면 엄격한 Repository/Service 추상화, 응답 enum, trait 또는 generic을 추가하면 변환과 오류 경로까지 함께 바뀌어 S05의 동작 보존을 확인하기 어려워집니다. 이들은 이번에 도입하지 않았습니다.

나중에 명령 수가 늘어 `build_reply()`를 읽기 어려워지면 명령별 함수 분리를 검토할 수 있습니다. 저장소 연산을 여러 장소에서 재사용해야 한다면 명령 단위의 원자성을 유지하는 API가 필요한지 검토할 수 있습니다. 지금의 `db.rs` 공개 필드와 command의 직접 접근은 단순한 구조를 유지하기 위한 선택입니다.

**검증 결과**

Windows 샌드박스에서는 Cargo/Redis 실행 파일을 찾지 못하고 WSL 접근도 거부되어, 사용자가 WSL에서 제공된 검증 스크립트를 실행했습니다. 실제 저장된 로그를 확인했습니다. 환경은 WSL2 Linux x86_64, rustc/cargo 1.98.1, Redis 서버·CLI 8.10.2입니다. CI에 지정된 Redis 8.10.2와 같습니다. 별도 포트 16379/16380을 사용했습니다.

| 검증 | 원본 S05 | 리팩터링 후 |
| --- | --- | --- |
| `cargo fmt --check` | 통과 | 통과 |
| `cargo clippy -- -D warnings` | 통과 | 통과 |
| CI 범위 `cargo clippy --all-targets -- -D warnings` | 통과 | 통과 |
| `cargo test` | 6개 통과 | 13개 통과 |
| `./scripts/difftest.sh` | 73개 통과 / 0개 실패 | 73개 통과 / 0개 실패 |

첫 실행에서 신규 테스트 두 곳의 포맷 차이를 수정했습니다. difftest는 기존 shell 스크립트와 케이스 파일의 CRLF 때문에 먼저 실패했습니다. 스크립트만 LF로 바꾼 임시 검증에서는 케이스 끝의 CR이 명령에 들어가 전후 동일하게 28건 실패했습니다. 최종 검증은 **양쪽 임시 복사본의 스크립트와 케이스 파일만 LF로 통일**한 결과입니다. 원본 저장소의 스크립트와 케이스 파일은 보존했으므로, 그 폴더에서 WSL로 직접 실행할 때는 기존 CRLF 문제가 남아 있습니다.

추가한 7개 테스트는 바이너리와 중복 키 처리, INCR 계열의 TTL 유지와 오류 시 값 보존, 정수 범위 오류, 만료된 키의 명령별 취급, SET 옵션과 EXPIRE/PERSIST, 4개 스레드의 400회 INCR, 파이프라인의 소비 바이트와 바이너리 인자를 확인합니다. 만료 테스트는 과거/현재 시각의 엔트리를 직접 준비해 대기 시간에 의존하지 않습니다.

원본의 main 이외 모든 기존 함수 본문과 기존 6개 테스트 본문을 자동 대조해 동일함을 확인했습니다. main의 연결 수락 루프도 그대로 새 함수로 이동했습니다. Cargo.toml/Cargo.lock, 기존 difftest와 케이스 파일은 변경하지 않았습니다. 새로운 외부 의존성은 없습니다.

**발견했지만 수정하지 않은 기존 문제와 검증 한계**

- `parse_array()`는 bulk 본문 뒤에 2바이트가 도착했는지만 보고 실제 CRLF인지 검사하지 않습니다. 예를 들어 `*1\r\n$4\r\nPINGxx`도 완성된 명령으로 처리되는 코드입니다.
- 만료 시각 계산에 `Instant::now() + duration`을 사용합니다. 플랫폼이 표현할 수 없는 큰 유효 정수 입력에 대한 checked_add 처리가 없습니다. 경계 입력 처리는 별도 수정 과제로 남겼습니다.
- difftest가 파일마다 보내는 `FLUSHALL`은 현재 구현에 없고 그 오류도 버립니다. 따라서 clone의 데이터가 케이스 파일 사이에 남을 수 있습니다. 이번 리팩터링에서 FLUSHALL을 추가하지 않았습니다.
- difftest는 redis-cli 출력 문자열을 비교하므로 원시 RESP 전체의 바이트 동등성을 증명하는 테스트는 아닙니다. 바이트 동작 보존은 기존 응답 코드의 동일성 대조와 단위 테스트의 바이트 비교로 추가 확인했으며, 모든 입력을 망라한 Redis 호환성을 주장하지 않습니다.

**S06에서 다시 볼 위치**

새 명령 처리와 관련 테스트는 `command.rs`, 저장 값의 표현은 `db.rs`에서 살펴보면 됩니다. 새 응답 형식이 필요하면 `reply.rs`, 비교 케이스는 `tests/cases/`가 변경 후보입니다. 현재 RESP 배열 요청을 계속 쓰는 한 `resp.rs`와 `connection.rs`를 바꿀 필요가 있는지부터 확인하면 됩니다. 이번에는 List 자료구조, Value enum, VecDeque를 설계하거나 구현하지 않았습니다.

학습 확인용 질문: `Arc::clone()`이 HashMap 복사가 아닌 이유, INCR에서 잠금을 두 번 나누면 생기는 문제, `mod`와 `use`의 차이, parse_array를 pub으로 만들지 않은 이유를 현재 코드에 연결해 설명해 보세요.
