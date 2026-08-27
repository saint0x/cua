type ProcessEnv = Record<string, string | undefined>;

declare const process: {
  env: ProcessEnv;
};

declare module "node:child_process" {
  export function spawn(
    bin: string,
    args: string[],
    options: { env: ProcessEnv },
  ): {
    stdout: {
      setEncoding(encoding: string): void;
      on(event: "data", callback: (chunk: string) => void): void;
    };
    stderr: {
      setEncoding(encoding: string): void;
      on(event: "data", callback: (chunk: string) => void): void;
    };
    on(event: "error", callback: (error: Error) => void): void;
    on(event: "close", callback: (code: number | null) => void): void;
  };
}

declare module "node:crypto" {
  export function randomUUID(): string;
}

declare module "node:fs/promises" {
  export function mkdir(path: string, options: { recursive: boolean }): Promise<void>;
  export function mkdtemp(prefix: string): Promise<string>;
  export function readFile(path: string, encoding: string): Promise<string>;
  export function rm(path: string, options: { force: boolean; recursive: boolean }): Promise<void>;
  export function writeFile(path: string, data: string, encoding: string): Promise<void>;
}

declare module "node:os" {
  export function homedir(): string;
  export function tmpdir(): string;
}

declare module "node:path" {
  export function join(...parts: string[]): string;
}

declare module "node:net" {
  export interface Socket {
    setEncoding(encoding: string): void;
    on(event: "data", callback: (chunk: string) => void): void;
    on(event: "end" | "close", callback: () => void): void;
    once(event: "connect", callback: () => void): void;
    once(event: "error", callback: (error: Error) => void): void;
    write(data: string): void;
    end(): void;
    destroy(): void;
  }

  export function createConnection(path: string): Socket;
}
