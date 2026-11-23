/**
 * Syki Lite Extension Platform API Type Definitions
 * Spec: syki_platform_design_v2 (Core v1 + Extended v1.x)
 */

export interface Disposable {
  dispose(): void;
}

export interface ProgressHandle {
  set(message: string, ratio?: number): void;
  done(): void;
}

export interface ActionPatch {
  label?: string;
  icon?: string;
  badge?: string;
  disabled?: boolean;
}

export interface ActionHandle extends Disposable {
  update(patch: ActionPatch): void;
}

export type SykiPermission =
  | 'llm'
  | 'decision'
  | 'voice'
  | 'meeting'
  | 'vision'
  | 'image'
  | 'semantic'
  | 'pty'
  | `shell:${string}`;

export type PolicyVerdict = 'allow' | 'deny' | 'ask';

export interface PolicyDecision {
  verdict: PolicyVerdict;
  ruleId: string;
  reason: string;
  subject: string;
  timestamp: string;
}

export interface AssetRef {
  id: string;
  url: string;
  mime: string;
  bytes: number;
}

export interface SearchHit {
  path: string;
  title?: string;
  snippet: string;
  score: number;
  line?: number;
  tags?: string[];
  updatedAt?: string;
}

export interface NoteMeta {
  path: string;
  title: string;
  tags: string[];
  updatedAt: string;
  isScrap: boolean;
}

export interface ShellResult {
  code: number;
  stdout: string;
  stderr: string;
  timedOut?: boolean;
  cancelled?: boolean;
}

export interface FormFieldOption {
  value: string;
  label: string;
}

export interface FormField {
  key: string;
  label: string;
  type: 'text' | 'textarea' | 'select' | 'checkbox' | 'number';
  defaultValue?: string | number | boolean;
  placeholder?: string;
  description?: string;
  required?: boolean;
  options?: FormFieldOption[];
}

export interface TabInfo {
  id: string;
  title: string;
  path?: string;
}

export interface SelectionRect {
  x: number;
  y: number;
  width: number;
  height: number;
}

export interface SykiPlugin {
  id: string;
  name: string;
  apiVersion: 1;
  /** Manifest permissions for one-click approval (e.g. ['llm', 'decision', 'shell:git']). */
  permissions?: SykiPermission[];
  init(syki: SykiAPI): void | Promise<void> | Disposable | Promise<Disposable>;
}

export interface SykiCapabilities {
  readonly llm: boolean;
  readonly decisionModel: boolean;
  readonly semanticSearch: boolean;
  readonly voice: boolean;
  readonly meeting: boolean;
  /** Extended v1.x capabilities */
  readonly vision?: boolean;
  readonly imageGen?: boolean;
  readonly screenCapture?: boolean;
}

export interface SykiEventMap {
  'editor:change': (e: { tabId: string }) => void;
  'selection:change': (e: { text: string; rect: DOMRect | null }) => void;
  'tab:change': (e: { tabId: string; path?: string }) => void;
  'theme:change': (e: { theme: 'ink' | 'paper' }) => void;
  'file:before-save': (
    e: { path: string; text: string }
  ) => { cancel: true; reason: string } | void | Promise<{ cancel: true; reason: string } | void>;
  'file:save': (e: { path: string }) => void;
  'file:drop': (
    e: { paths: string[]; mimes: string[] }
  ) => { handled: true } | void | Promise<{ handled: true } | void>;
  /** Fired when a field registered via `ui.registerSettings` changes. */
  'settings:change': (e: { key: string; value: unknown }) => void;
  /** Fired after Win32 Quick Capture saves a scrap (buffered up to 32 items while Detached). */
  'quick-capture:saved': (e: { text: string; assetPaths: string[]; scrapPath: string }) => void;
  /** Fired before CanvasHost transitions to Detached (500ms hard timeout). */
  'host:before-detach': () => void | Promise<void>;
  /** Extended v1.x: Fired when stdin is piped into `syki-cli`. */
  'pipe:in': (e: { text: string; source?: string }) => void | Promise<void>;
}

export interface SykiEditorAPI {
  getText(): string;
  setText(text: string): void;
  getSelection(): string;
  getSelectionRange(): { from: number; to: number };
  getSelectionRect(): SelectionRect | null;
  insert(text: string): void;
  replaceSelection(text: string): void;
  replaceRange(from: number, to: number, text: string): void;
  openTab(opts: { title?: string; content: string }): void;
  getActiveTab(): TabInfo;
  listTabs(): TabInfo[];
  getTabText(tabId: string): string | null;
}

export interface SykiKnowledgeAPI {
  searchSemantic(query: string, opts?: { limit?: number }): Promise<SearchHit[]>;
  searchFulltext(
    query: string,
    opts?: { limit?: number; tag?: string; dateFrom?: string }
  ): Promise<SearchHit[]>;
  updateSemanticIndex(): Promise<{ files: number; chunks: number }>;
  notes: {
    read(path: string): Promise<string>;
    list(dir?: string): Promise<NoteMeta[]>;
    create(opts: { title: string; content: string; folder?: string }): Promise<string>;
    append(path: string, text: string): Promise<void>;
    update(path: string, content: string): Promise<void>;
  };
  assets: {
    save(asset: Blob | AssetRef, ext?: string): Promise<string>;
  };
}

export interface SykiExecRenderAPI {
  mermaidToPng(
    code: string,
    opts?: { theme?: 'ink' | 'paper'; scale?: number; signal?: AbortSignal }
  ): Promise<AssetRef>;
  htmlToPng(
    html: string,
    opts?: { width?: number; height?: number; scale?: number; signal?: AbortSignal }
  ): Promise<AssetRef>;
  htmlToPdf(
    html: string,
    opts?: { landscape?: boolean; printBackground?: boolean; signal?: AbortSignal }
  ): Promise<AssetRef>;
}

export interface SykiExecAPI {
  /** Calls the built-in LLM. `stripSecrets` defaults to `true`. */
  llm(
    prompt: string,
    opts?: { input?: string; system?: string; stripSecrets?: boolean; signal?: AbortSignal }
  ): Promise<string>;
  /** Executes an approved binary with an argument array (no shell interpolation). */
  shell(
    program: string,
    args?: string[],
    opts?: { stdin?: string; cwd?: string; timeoutMs?: number; signal?: AbortSignal }
  ): Promise<ShellResult>;
  pty: {
    open(opts?: { cwd?: string; command?: string; worktree?: boolean }): Promise<string>;
  };
  /** Extended v1.x: Vision analysis over an AssetRef or Blob. */
  vision(
    asset: AssetRef | Blob,
    prompt: string,
    opts?: { signal?: AbortSignal }
  ): Promise<string>;
  /** Extended v1.x: Image generation returning a zero-copy AssetRef. */
  generateImage(
    prompt: string,
    opts?: { size?: string; signal?: AbortSignal }
  ): Promise<AssetRef>;
  /** Extended v1.x: Off-screen diagram & HTML renderers. */
  render: SykiExecRenderAPI;
}

export interface SykiUIAPI {
  registerAction(action: {
    id: string;
    label: string;
    icon?: string;
    badge?: string;
    slot: 'toolbar' | 'statusbar' | 'context-menu';
    order?: number;
    priority?: 'primary' | 'secondary';
    concurrency?: 'lock' | 'cancel-prev' | 'allow';
    run: (ctx: { progress: ProgressHandle; signal: AbortSignal }) => void | Promise<void>;
  }): ActionHandle;
  mountPanel(
    slot: 'side-panel' | 'floating' | 'modal',
    opts: { title?: string; render: (root: ShadowRoot) => void; x?: number; y?: number }
  ): Disposable;
  registerSettings(schema: FormField[]): Disposable;
  form<T extends Record<string, unknown>>(schema: FormField[]): Promise<T | null>;
  decide(opts: {
    title: string;
    question?: string;
    context?: string;
    options: Array<{ id: string; label: string; description?: string; recommended?: boolean }>;
    allowCustom?: boolean;
  }): Promise<{ choiceId: string; customText?: string } | null>;
  review(text: string, opts?: { title?: string; okLabel?: string }): Promise<string | null>;
  diffConfirm(before: string, after: string, opts?: { title?: string }): Promise<boolean>;
  toast(message: string, level?: 'info' | 'ok' | 'warn' | 'error'): void;
  getTheme(): 'ink' | 'paper';
  setToken(name: `--${string}`, value: string): Disposable;
  setChrome(opts: {
    toolbar?: boolean;
    tabs?: boolean;
    statusbar?: boolean;
    zen?: boolean;
  }): Disposable;
}

export interface SykiFlowAPI {
  steps<T>(
    initial: T,
    steps: Array<{ label: string; run: (val: any, signal: AbortSignal) => Promise<any> }>,
    opts?: { progress?: ProgressHandle; signal?: AbortSignal }
  ): Promise<any>;
  parallel<T extends readonly Promise<unknown>[]>(
    tasks: T
  ): Promise<{ [K in keyof T]: Awaited<T[K]> }>;
  decide<Id extends string>(opts: {
    input: string;
    choices: Array<{ id: Id; description: string }>;
    rule?: (input: string) => Id | null;
    engine?: 'decision' | 'llm';
    prompt?: string;
    stripSecrets?: boolean;
    fallback: Id;
    signal?: AbortSignal;
  }): Promise<{ choiceId: Id; reason?: string; by: 'rule' | 'decision' | 'llm' | 'fallback' }>;
  /** Extended v1.x: Bounded iterative loop (hard cap: maxTurns <= 10). */
  loop<T>(opts: {
    init: T;
    maxTurns: number;
    step: (state: T, turn: number, signal: AbortSignal) => Promise<T>;
    until: (state: T, turn: number) => boolean | Promise<boolean>;
    progress?: ProgressHandle;
    signal?: AbortSignal;
  }): Promise<{ state: T; turns: number; completed: boolean }>;
}

/** Extended v1.x: Multimodal input capture APIs. */
export interface SykiInputAPI {
  voice(opts?: { signal?: AbortSignal }): Promise<string | null>;
  meeting(opts?: { signal?: AbortSignal }): Promise<{ transcript: string; path?: string } | null>;
  captureScreen(opts?: { signal?: AbortSignal }): Promise<AssetRef | null>;
}

export interface SykiSysAPI {
  readonly locale: 'ja' | 'en';
  on<K extends keyof SykiEventMap>(event: K, handler: SykiEventMap[K]): Disposable;
  registerCommand(cmd: {
    id: string;
    title: string;
    shortcut?: string;
    exposeRpc?: boolean;
    run: (args?: unknown) => unknown | Promise<unknown>;
  }): Disposable;
  runCommand(commandId: string, args?: unknown): Promise<unknown>;
  stripSecrets(text: string): string;
  clipboard: {
    read(): Promise<{ text?: string; imageBlob?: Blob; imageAsset?: AssetRef }>;
    writeText(text: string): Promise<void>;
    writeImage(asset: Blob | AssetRef): Promise<void>;
  };
  pickFile(opts?: { extensions?: string[] }): Promise<string | null>;
  storage: {
    get<T = unknown>(key: string, fallback?: T): Promise<T>;
    set(key: string, value: unknown): Promise<void>;
  };
}

export interface SykiAPI {
  readonly apiVersion: 1;
  readonly capabilities: Readonly<SykiCapabilities>;
  readonly editor: SykiEditorAPI;
  readonly knowledge: SykiKnowledgeAPI;
  readonly exec: SykiExecAPI;
  readonly ui: SykiUIAPI;
  readonly flow: SykiFlowAPI;
  readonly input: SykiInputAPI;
  readonly sys: SykiSysAPI;
}

declare global {
  interface Window {
    readonly syki?: SykiAPI;
  }
}
