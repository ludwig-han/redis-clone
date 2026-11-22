//! TCP나 저장소에 접근하지 않고 RESP 바이트를 명령으로 해석합니다.

use crate::number::parse_number;

/// 버퍼 앞쪽에서 명령 하나를 꺼낸 결과입니다.
pub(super) enum Parsed {
    /// 명령 하나와, 그 명령이 버퍼에서 차지한 바이트 수
    Complete(Vec<Vec<u8>>, usize),
    /// 아직 덜 왔습니다
    Incomplete,
    /// 형식이 깨졌습니다
    Broken,
}

/// redis-cli 는 명령을 RESP 배열로 보냅니다: `*2\r\n$4\r\nPING\r\n$5\r\nhello\r\n`
/// 진짜 Redis는 `nc` 로 친 생 텍스트(인라인 명령)도 받아줍니다.
pub(super) fn parse_command(buffer: &[u8]) -> Parsed {
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn 파이프라인은_소비한_바이트_뒤에서_계속_파싱한다() {
        let buffer = b"*1\r\n$4\r\nPING\r\n*2\r\n$4\r\nECHO\r\n$3\r\na\0b\r\n";
        let (first, consumed) = complete(buffer);
        assert_eq!(first, vec![b"PING".to_vec()]);
        let (second, rest) = complete(&buffer[consumed..]);
        assert_eq!(second, vec![b"ECHO".to_vec(), b"a\0b".to_vec()]);
        assert_eq!(consumed + rest, buffer.len());
    }
}
