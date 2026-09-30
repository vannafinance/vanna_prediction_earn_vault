function write(level: string, message: string): void {
  const line = `${new Date().toISOString()} ${level.padEnd(5)} ${message}`;
  if (level === "ERROR" || level === "WARN") console.error(line);
  else console.log(line);
}

export const log = {
  info: (message: string) => write("INFO", message),
  warn: (message: string) => write("WARN", message),
  error: (message: string) => write("ERROR", message),
};
