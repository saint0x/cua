export type Json = null | boolean | number | string | Json[] | {
    [key: string]: Json;
};
export type CuaEnv = Record<string, string | undefined>;
export interface CuaOptions {
    profile?: string;
    bin?: string;
    env?: CuaEnv;
}
export interface RunebookOptions {
    traceDir?: string;
}
export interface RpcOptions {
    sessionId?: string;
}
export interface OwnerSession {
    sessionId: string;
    raw: Json;
}
export declare class Cua {
    private readonly profile;
    private readonly bin;
    private readonly env;
    private constructor();
    static connect(options?: CuaOptions): Promise<Cua>;
    run(file: string, options?: RunebookOptions): Promise<Json>;
    runInline(runebookToml: string, options?: RunebookOptions): Promise<Json>;
    rpc(method: string, params?: Json, options?: RpcOptions): Promise<Json>;
    status(): Promise<Json>;
    configStatus(): Promise<Json>;
    acquireOwner(clientName?: string, ttlMs?: number): Promise<OwnerSession>;
    private execJson;
}
