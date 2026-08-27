import { Socket } from "node:net";
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
export interface RunStep {
    id?: string;
    do: string;
    save_as?: string;
    on_error?: "stop" | "continue" | "ask" | "rollback";
    [key: string]: Json | undefined;
}
export interface RunStepsOptions extends RunebookOptions {
    name?: string;
    trace?: boolean;
    onError?: "stop" | "continue" | "ask" | "rollback";
}
export interface ScreenshotOptions {
    maxWidth?: number;
    encoding?: "png" | "jpeg";
    forceFresh?: boolean;
    includeBytes?: boolean;
}
export interface VisualSessionOptions {
    maxWidth?: number;
    fps?: number;
    includeBytes?: boolean;
    durationMs?: number;
    queueDepth?: number;
    session?: OwnerSession | string;
}
export interface WindowCaptureOptions {
    windowId: number;
    maxWidth?: number;
    encoding?: "png" | "jpeg";
    includeBytes?: boolean;
}
export interface EventsOptions {
    after?: number;
    timeoutMs?: number;
}
export interface UiStepOptions {
    label: string;
    source?: string;
    task?: string;
    tool?: string;
    stepIndex?: number;
    stepTotal?: number;
    ttlMs?: number;
}
export interface UiReplyOptions {
    text: string;
    source?: string;
    ttlMs?: number;
}
export interface ProfileCreateOptions {
    name: string;
    mode?: "observe" | "supervised" | "autonomous";
    durationMs?: number;
    capabilities?: Json;
}
export interface DispatchOptions {
    session?: OwnerSession | string;
}
export interface DispatchFrameOptions extends DispatchOptions {
    sourceFrame: Json;
    action: Json;
}
export declare class Cua {
    private readonly profile;
    private readonly bin;
    private readonly env;
    private constructor();
    static connect(options?: CuaOptions): Promise<Cua>;
    run(file: string, options?: RunebookOptions): Promise<Json>;
    runInline(runebookToml: string, options?: RunebookOptions): Promise<Json>;
    runSteps(steps: RunStep[], options?: RunStepsOptions): Promise<Json>;
    rpc(method: string, params?: Json, options?: RpcOptions): Promise<Json>;
    manifest(): Promise<Json>;
    schemas(): Promise<Json>;
    metrics(): Promise<Json>;
    status(): Promise<Json>;
    configStatus(): Promise<Json>;
    acquireOwner(clientName?: string, ttlMs?: number): Promise<OwnerSession>;
    cancelSession(session: OwnerSession | string, targetSessionId?: string): Promise<Json>;
    sessionStatus(): Promise<Json>;
    profileStatus(): Promise<Json>;
    createProfile(options: ProfileCreateOptions, session?: OwnerSession | string): Promise<Json>;
    activateProfile(session?: OwnerSession | string): Promise<Json>;
    requestAccessibility(): Promise<Json>;
    observe(): Promise<Json>;
    screenshot(options?: ScreenshotOptions): Promise<Json>;
    windowCapture(options: WindowCaptureOptions): Promise<Json>;
    context(options?: ScreenshotOptions): Promise<Json>;
    events(options?: EventsOptions): Promise<Json>;
    uiStep(options: UiStepOptions): Promise<Json>;
    uiIsland(state: "expanded" | "collapsed" | "toggle", source?: string): Promise<Json>;
    uiReply(options: UiReplyOptions): Promise<Json>;
    uiMode(mode: "headful" | "headless", source?: string): Promise<Json>;
    clipboardRead(allowSensitive?: boolean): Promise<Json>;
    clipboardWrite(text: string, session?: OwnerSession | string): Promise<Json>;
    pause(session: OwnerSession | string): Promise<Json>;
    resume(session: OwnerSession | string): Promise<Json>;
    killSwitch(session: OwnerSession | string): Promise<Json>;
    dispatch(action: Json, options?: DispatchOptions): Promise<Json>;
    dispatchFrame(options: DispatchFrameOptions): Promise<Json>;
    visualSession(options?: VisualSessionOptions): Promise<CuaVisualSession>;
    openApp(app: string, options?: DispatchOptions): Promise<Json>;
    shell(command: string, options?: DispatchOptions): Promise<Json>;
    aegis(args: string[], options?: DispatchOptions): Promise<Json>;
    ctx(args: string[], options?: DispatchOptions): Promise<Json>;
    private step;
    private execJson;
}
export declare class CuaVisualSession {
    private readonly socket;
    private buffer;
    private messages;
    private waiters;
    private ended;
    private closeSent;
    constructor(socket: Socket);
    nextMessage(): Promise<Json | null>;
    nextFrame(): Promise<Json | null>;
    close(): Promise<void>;
    cancel(): Promise<void>;
    private receive;
    private push;
    private finish;
}
