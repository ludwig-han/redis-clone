"""Compare raw RESP2 replies with an isolated Redis server; standard library only."""
import socket
import subprocess
import sys
import time

REF_PORT, OUR_PORT = 16381, 16382


def encode(command):
    return b"*%d\r\n" % len(command) + b"".join(
        b"$%d\r\n" % len(arg) + arg + b"\r\n" for arg in command
    )


def read_reply(reader):
    line = reader.readline()
    if not line.endswith(b"\r\n"):
        raise AssertionError(f"Incomplete RESP header: {line!r}")
    kind = line[:1]
    if kind == b"$":
        length = int(line[1:-2])
        if length >= 0:
            data = reader.read(length + 2)
            if len(data) != length + 2 or not data.endswith(b"\r\n"):
                raise AssertionError(f"Incomplete bulk: {data!r}")
            return line + data
    elif kind == b"*":
        return line + b"".join(read_reply(reader) for _ in range(max(0, int(line[1:-2]))))
    elif kind not in (b"+", b"-", b":"):
        raise AssertionError(f"Unexpected RESP type: {line!r}")
    return line


def wait_ready(port, process):
    for _ in range(100):
        if process.poll() is not None:
            raise RuntimeError(f"Server exited: {process.stderr.read().decode(errors='replace')}")
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=0.2):
                return
        except OSError:
            time.sleep(0.05)
    raise RuntimeError(f"Server did not start on {port}")


def main():
    for port in (REF_PORT, OUR_PORT):
        with socket.socket() as probe:
            if probe.connect_ex(("127.0.0.1", port)) == 0:
                raise RuntimeError(f"Port {port} already in use; refusing to reuse a server")
    processes = []
    try:
        for args, port in [
            (["redis-server", "--bind", "127.0.0.1", "--port", str(REF_PORT), "--save", "", "--appendonly", "no"], REF_PORT),
            ([sys.argv[1], "--port", str(OUR_PORT)], OUR_PORT),
        ]:
            process = subprocess.Popen(args, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
            processes.append(process)
            wait_ready(port, process)

        with socket.create_connection(("127.0.0.1", REF_PORT), timeout=3) as ref, socket.create_connection(("127.0.0.1", OUR_PORT), timeout=3) as ours:
            with ref.makefile("rb") as ref_reader, ours.makefile("rb") as our_reader:
                count = 0

                def compare(command):
                    nonlocal count
                    packet = encode(command)
                    ref.sendall(packet)
                    ours.sendall(packet)
                    expected, actual = read_reply(ref_reader), read_reply(our_reader)
                    if expected != actual:
                        raise AssertionError(f"{command!r}\nRedis: {expected!r}\nClone: {actual!r}")
                    count += 1

                binary = b"\x00\xffkey"
                commands = [
                    [b"LPUSH", binary, b"", b"a\r\nb", b"\x00\xff"],
                    [b"RPUSH", binary, "한글".encode(), b"tail"],
                    [b"LRANGE", binary, b"0", b"-1"],
                    [b"LLEN", binary],
                    [b"LPOP", binary],
                    [b"RPOP", binary, b"2"],
                    [b"LPOP", binary, b"0"],
                    [b"LRANGE", binary, b"0", b"-1"],
                    [b"LPOP", binary, b"9223372036854775807"],
                    [b"EXISTS", binary],
                    [b"LPOP", binary],
                    [b"RPOP", binary, b"1"],
                    [b"LPOP", binary, b"0"],
                    [b"LRANGE", binary, b"0", b"-1"],
                    [b"LLEN", binary],
                    [b"RPUSH", b"range", b"a", b"b", b"c"],
                ]
                for command in commands:
                    compare(command)
                indexes = [b"-9223372036854775808", b"-99", b"-4", b"-3", b"-2", b"-1", b"0", b"1", b"2", b"3", b"99", b"9223372036854775807"]
                for start in indexes:
                    for stop in indexes:
                        compare([b"LRANGE", b"range", start, stop])
                compare([b"SET", b"wrong", b"original"])
                for key in (b"range", b"wrong", b"missing"):
                    for bad in (b"hello", b"+1", b"01", b"-0", b"", b" 1", b"1 ", b"9223372036854775808"):
                        compare([b"LRANGE", key, bad, b"-1"])
                        compare([b"LPOP", key, bad])
                        compare([b"RPOP", key, bad])
                    for command in ([b"LPOP", key, b"-1"], [b"LPOP", key, b"0"], [b"RPOP", key, b"0"], [b"LRANGE", key, b"0", b"-1"], [b"LLEN", key]):
                        compare(command)

                # One TCP write containing multiple commands; preserve reply order and bytes.
                pipeline = [
                    [b"RPUSH", b"pipe", b"a", b"b"],
                    [b"LPUSH", b"pipe", b"c"],
                    [b"LRANGE", b"pipe", b"0", b"-1"],
                    [b"RPOP", b"pipe", b"2"],
                    [b"LPOP", b"pipe"],
                    [b"EXISTS", b"pipe"],
                ]
                packet = b"".join(encode(command) for command in pipeline)
                ref.sendall(packet)
                ours.sendall(packet)
                for command in pipeline:
                    expected, actual = read_reply(ref_reader), read_reply(our_reader)
                    assert expected == actual, (command, expected, actual)
                    count += 1
                print(f"Raw RESP byte comparisons: {count} passed, 0 failed (including binary, nulls, pipeline, index extremes)")
    finally:
        for process in reversed(processes):
            if process.poll() is None:
                process.terminate()
            try:
                process.communicate(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.communicate()


if __name__ == "__main__":
    main()
