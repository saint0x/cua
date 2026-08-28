import { Socket } from "node:net";
export type Json = null | boolean | number | string | Json[] | {
    [key: string]: Json;
};
export type CuaEnv = Record<string, string | undefined>;
export interface CuaOptions {
    profile?: string;
    bin?: string;
    env?: CuaEnv;
    transport?: "unix" | "cli";
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
    frames?: number;
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
export interface InboundMessageOptions {
    source?: string;
    idempotencyKey?: string;
    payload?: Json;
    replyUrl?: string;
    ttlMs?: number;
}
export interface WebhookMessageOptions extends InboundMessageOptions {
    source: string;
}
export interface WebhookSubscribeOptions {
    source: string;
    secret?: string;
    replyUrl?: string;
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
export interface UiSceneOptions {
    scene: Json;
    source?: string;
}
export interface UiSceneThemeOptions {
    theme: Json;
    source?: string;
}
export interface UiSceneBackgroundOptions {
    background: Json;
    source?: string;
}
export interface ProfileCreateOptions {
    name: string;
    mode?: "observe" | "supervised" | "autonomous";
    durationMs?: number;
    capabilities?: Json;
}
export interface DispatchOptions {
    session: OwnerSession | string;
}
export interface DispatchFrameOptions extends DispatchOptions {
    sourceFrame: Json;
    action: Json;
}
export interface ModelEvalOptions {
    live?: boolean;
    maxCalls?: number;
    maxOutputTokens?: number;
}
export declare class Cua {
    private readonly profile;
    private readonly bin;
    private readonly env;
    private readonly transport;
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
    attest(audience: string, nonce: string, session?: OwnerSession | string): Promise<Json>;
    acquireOwner(clientName?: string, ttlMs?: number): Promise<OwnerSession>;
    cancelSession(session: OwnerSession | string, targetSessionId?: string): Promise<Json>;
    heartbeatOwner(session: OwnerSession | string, ttlMs?: number): Promise<OwnerSession>;
    sessionStatus(): Promise<Json>;
    inboxPublish(text: string, options?: InboundMessageOptions): Promise<Json>;
    inboxAfter(afterSequence?: number): Promise<Json>;
    inboxStatus(messageId: string): Promise<Json>;
    webhookPublish(text: string, options: WebhookMessageOptions): Promise<Json>;
    webhookSubscribe(options: WebhookSubscribeOptions): Promise<Json>;
    webhookStatus(source: string): Promise<Json>;
    scratchpadWrite(name: string, text: string, session: OwnerSession | string, options?: {
        durable?: boolean;
        append?: boolean;
        ttlMs?: number;
    }): Promise<Json>;
    scratchpadRead(name: string, durable?: boolean): Promise<Json>;
    scratchpadList(options?: {
        includeDurable?: boolean;
        includeEphemeral?: boolean;
    }): Promise<Json>;
    scratchpadDelete(name: string, session: OwnerSession | string, options?: {
        durable?: boolean;
        ephemeral?: boolean;
    }): Promise<Json>;
    profileStatus(): Promise<Json>;
    createProfile(options: ProfileCreateOptions, session: OwnerSession | string): Promise<Json>;
    activateProfile(session: OwnerSession | string): Promise<Json>;
    requestAccessibility(): Promise<Json>;
    observe(): Promise<Json>;
    screenshot(options?: ScreenshotOptions): Promise<Json>;
    windowCapture(options: WindowCaptureOptions): Promise<Json>;
    context(options?: ScreenshotOptions): Promise<Json>;
    events(options?: EventsOptions): Promise<Json>;
    visualFrames(options?: VisualSessionOptions): Promise<Json>;
    uiStep(options: UiStepOptions): Promise<Json>;
    uiIsland(state: "expanded" | "collapsed" | "toggle", source?: string): Promise<Json>;
    uiSceneSet(options: UiSceneOptions): Promise<Json>;
    uiScenePatch(options: UiSceneOptions): Promise<Json>;
    uiSceneReset(source?: string): Promise<Json>;
    uiSceneTheme(options: UiSceneThemeOptions): Promise<Json>;
    uiSceneBackground(options: UiSceneBackgroundOptions): Promise<Json>;
    uiReply(options: UiReplyOptions): Promise<Json>;
    uiMode(mode: "headful" | "headless", source?: string): Promise<Json>;
    clipboardRead(allowSensitive?: boolean): Promise<Json>;
    clipboardWrite(text: string, session: OwnerSession | string): Promise<Json>;
    pause(session: OwnerSession | string): Promise<Json>;
    resume(session: OwnerSession | string): Promise<Json>;
    killSwitch(session: OwnerSession | string): Promise<Json>;
    dispatch(action: Json, options: DispatchOptions): Promise<Json>;
    dispatchFrame(options: DispatchFrameOptions): Promise<Json>;
    visualSession(options?: VisualSessionOptions): Promise<CuaVisualSession>;
    openApp(app: string, options: DispatchOptions): Promise<Json>;
    shell(command: string, options: DispatchOptions): Promise<Json>;
    aegis(args: string[], options: DispatchOptions): Promise<Json>;
    ctx(args: string[], options: DispatchOptions): Promise<Json>;
    traceVerify(dir: string): Promise<Json>;
    traceReplay(dir: string, dryRun?: boolean): Promise<Json>;
    modelEval(options?: ModelEvalOptions): Promise<Json>;
    private step;
    private execJson;
    private unixRpc;
    private loadToken;
}
export declare class CuaProtocolError extends Error {
    readonly method: string;
    readonly error: Json;
    constructor(method: string, error: Json);
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
