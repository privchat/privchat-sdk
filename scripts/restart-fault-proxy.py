#!/usr/bin/env python3
"""Transparent TLS pass-through; only test sender downstream can be blackholed."""
import asyncio
import pathlib
import sys

directory = pathlib.Path(sys.argv[1])
listen = int(sys.argv[2])
target = int(sys.argv[3])
armed = False


async def arm():
    global armed
    while not (directory / "arm").exists():
        await asyncio.sleep(0.025)
    armed = True
    (directory / "armed").touch()


async def copy(reader, writer, downstream):
    while data := await reader.read(65536):
        if downstream and armed:
            (directory / "dropped-downstream").touch()
        else:
            writer.write(data)
            await writer.drain()


async def handle(client_reader, client_writer):
    server_writer = None
    try:
        server_reader, server_writer = await asyncio.open_connection("127.0.0.1", target)
        pipes = [
            asyncio.create_task(copy(client_reader, server_writer, False)),
            asyncio.create_task(copy(server_reader, client_writer, True)),
        ]
        _, pending = await asyncio.wait(pipes, return_when=asyncio.FIRST_COMPLETED)
        for task in pending:
            task.cancel()
        await asyncio.gather(*pipes, return_exceptions=True)
    finally:
        client_writer.close()
        if server_writer is not None:
            server_writer.close()


async def main():
    server = await asyncio.start_server(handle, "127.0.0.1", listen)
    (directory / "proxy-ready").touch()
    async with server:
        await asyncio.gather(server.serve_forever(), arm())


asyncio.run(main())
