# S02 RESP 파서 + ECHO — 한호택

- 구현 브랜치: `hht/s02-echo`
- 걸린 시간: 약 3시간

## 1. 무엇을 만들었나

RESP Array로 들어온 명령을 파싱하고 `ECHO <message>`에 응답하도록 구현했습니다.

또한 하나의 TCP 연결에서 여러 명령이 연속으로 들어오는 파이프라이닝을 처리하도록 수정했습니다.

- ECHO: <https://redis.io/docs/latest/commands/echo/>
- RESP: <https://redis.io/docs/latest/develop/reference/protocol-spec/>

## 2. 설계 선택 — 이 문서의 핵심

| 갈림길 | 고른 것 | 이유 | 버린 선택지와 그 이유 |
| --- | --- | --- | --- |
| 파서 에러 표현 | 직접 만든 `enum` 유지 | `Complete`, `Incomplete`, `Broken` 상태를 명확하게 구분할 수 있고, 기존 main 코드와도 자연스럽게 이어짐 | `anyhow`: 에러 처리는 편하지만 이번에는 파서의 상태를 직접 구분하는 것이 학습 목적에 더 맞다고 판단 |
| TCP 데이터 버퍼 | `Vec<u8>`에 누적 | TCP 데이터가 쪼개져 들어와도 buffer에 계속 모은 뒤 완성된 명령만 파싱할 수 있음 | `BufReader + read_line/read_exact`: S01에서 사용해봤지만, 여러 명령이 한꺼번에 들어오는 pipeline 처리에는 누적 buffer 방식이 더 직관적이라고 판단 |
| 파이프라인 처리 | buffer 안에서 반복해서 파싱하는 내부 `loop` 추가 | 한 번 읽은 buffer 안에 명령이 여러 개 있으면 다시 `read()`하지 않고 남은 명령까지 처리할 수 있음 | 명령 하나 처리할 때마다 다시 `read()`: 이미 buffer에 다음 명령이 있어도 새 입력을 기다릴 수 있음 |

**나중에 이 선택을 뒤집어야 하는 조건:**

RESP 형식이나 에러 종류가 많아져 `Broken` 하나만으로 원인을 구분하기 어려워지면 파서 에러 `enum`을 더 세분화할 수 있습니다.

버퍼를 계속 `drain`하는 방식이 성능상 문제가 되는 규모가 되면 버퍼 관리 방식도 다시 검토할 수 있습니다.

## 3. AI를 어떻게 썼나

- **첫 프롬프트에 뭐라고 썼나:**  
  S02를 시작하기 전에 현재 main의 S01 코드를 다시 읽고, S02에서 어떤 부분을 수정해야 하는지 설명해 달라고 요청했습니다.

- **AI 제안 중 거절한 것과 이유:**  
  구현 전에 기존 `parse_array` 코드를 너무 깊게 분석하는 것은 중단했습니다. S02 구현에 필요한 흐름만 이해한 뒤 직접 수정하는 것이 더 효율적이라고 판단했습니다.

- **AI가 틀렸던 것 / 스펙과 달랐던 것:**  
  특별히 발견한 것은 없었습니다.

- **이해하지 못한 채 넘어간 코드:**  
  `parse_array` 등의 함수 내부의 일부 Rust 문법은 완전히 이해하지는 못했습니다. 다만 buffer에서 RESP Array 하나를 읽어 `Vec<Vec<u8>>`로 만드는 흐름은 이해했습니다.

- **AI 없이 내가 결정한 것:**  
  기존 main 구조를 크게 바꾸지 않고 S02 범위인 ECHO와 pipeline까지만 구현하고, 이후 세션 기능을 미리 넣지 않았습니다.

## 4. 내가 추가한 테스트 케이스

`tests/cases/02-echo.txt`

```text
ECHO hello
echo hello
ECHO 한글
ECHO
ECHO a b
```

추가로 Rust 단위 테스트에 ECHO 정상 응답, 대소문자, UTF-8 문자열, 잘못된 인자 개수 케이스를 추가했습니다.

- 왜 이 케이스를 넣었나:
  ECHO의 정상 응답뿐 아니라 대소문자 처리, UTF-8 문자열의 바이트 길이, 인자가 부족하거나 많은 경우의 에러 응답까지 진짜 Redis와 비교하기 위해 넣었습니다.
- 넣었다가 깨진 것:
  Rust의 b"..." 바이트 문자열에 한글을 직접 넣었을 때 must be ASCII 컴파일 에러가 발생했습니다. "한글".as_bytes()로 바꿨습니다.

또한 pipeline은 아래처럼 PING과 ECHO RESP를 하나의 TCP 연결로 연속 전송해 수동 검증했습니다.

명령어:

```sh
printf '*1\r\n$4\r\nPING\r\n*2\r\n$4\r\nECHO\r\n$5\r\nhello\r\n' | nc 127.0.0.1 6380
```

응답:

```sh
+PONG
$5
hello
```

## 5. 막혔던 곳

기존 코드는 명령 하나를 파싱한 뒤 다시 stream.read()로 돌아가는 구조였습니다.
pipeline에서는 이미 buffer 안에 다음 명령이 들어 있을 수 있으므로, buffer를 처리하는 내부 loop를 추가했습니다.
이때 기존 코드의:

```rust
Parsed::Incomplete => continue
```

를 그대로 사용하면 내부 loop만 계속 반복하게 됩니다.
따라서:

```rust
Parsed::Incomplete => break
```

로 바꾸어 내부 파싱 loop를 빠져나간 뒤 바깥 loop의 stream.read()로 돌아가도록 했습니다.
또한 한글 단위 테스트에서:

```rust
b"한글"
```

을 사용할 수 없다는 것을 확인했고:

```rust
"한글".as_bytes()
```

로 수정했습니다.

## 6. 새로 배운 Rust 개념

- b"..." 바이트 문자열 리터럴에는 ASCII만 직접 사용할 수 있고, UTF-8 문자열은 .as_bytes()로 바이트 표현을 얻을 수 있습니다.
- 중첩된 loop에서 continue는 현재 loop를 다시 시작하고, break는 현재 loop를 빠져나갑니다.
- ```Vec<u8>``` buffer에 TCP 데이터를 누적하고 처리한 부분만 drain하면 쪼개진 입력과 연속된 명령을 모두 처리할 수 있습니다.

## 7. 검증 결과

```sh
aek_han@DESKTOP-SAMSUNGLaptop:/mnt/c/Users/mcc91/onedrive/dev/redis-clone$ cd scripts/difftes/sh
bash: cd: scripts/difftes/sh: No such file or directory
hotaek_han@DESKTOP-SAMSUNGLaptop:/mnt/c/Users/mcc91/onedrive/dev/redis-clone$ ./scripts/difftest.sh 
▶ 정답지 Redis를 띄웁니다 (포트 6379)
▶ 우리 서버를 빌드합니다
   Compiling redis-clone v0.1.0 (/mnt/c/Users/mcc91/onedrive/dev/redis-clone)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 2.12s
▶ 우리 서버를 띄웁니다 (포트 6380)

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

------------------------------
통과 15 / 실패 0
```

pipeline 수동 검증:

```sh
PING + ECHO hello 연속 전송

+PONG
$5
hello
```

## 8. 다른 사람 문서를 읽고 (세션 직전에 채움)

- 나와 제일 다른 지점:
- 물어볼 것:
- 남의 케이스로 내 서버를 돌려본 결과:
