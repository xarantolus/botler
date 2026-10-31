// botler forwarder: relays 127.0.0.1:7420 inside the agent container to the hub's Unix socket,
// then runs the given command (pi). pi's MCP client, git smart-HTTP and the extension keep using
// plain http://127.0.0.1:7420, and the agent reaches the hub even with `--network none`.
// Usage: node forward.mjs <command> [args...]   (socket: $BOTLER_SOCKET)
import net from "node:net";
import { spawn } from "node:child_process";

const socket = process.env.BOTLER_SOCKET;
const [command, ...args] = process.argv.slice(2);
if (!socket || !command) {
  console.error("forward.mjs: BOTLER_SOCKET and a command are required");
  process.exit(2);
}

const server = net.createServer((client) => {
  const hub = net.connect(socket);
  const close = () => {
    client.destroy();
    hub.destroy();
  };
  client.on("error", close);
  hub.on("error", (e) => {
    console.error(`forward.mjs: hub socket ${socket}: ${e.message}`);
    close();
  });
  client.pipe(hub).pipe(client);
});

server.on("error", (e) => {
  console.error(`forward.mjs: cannot listen on 127.0.0.1:7420: ${e.message}`);
  process.exit(1);
});

server.listen(7420, "127.0.0.1", () => {
  const child = spawn(command, args, { stdio: "inherit" });
  child.on("error", (e) => {
    console.error(`forward.mjs: cannot start ${command}: ${e.message}`);
    process.exit(127);
  });
  child.on("exit", (code, signal) => process.exit(code ?? (signal ? 128 : 1)));
  for (const s of ["SIGTERM", "SIGINT", "SIGHUP"]) process.on(s, () => child.kill(s));
});
