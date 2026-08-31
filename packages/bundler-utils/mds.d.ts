/** A single structured message from a messages-mode MDS compile result. */
interface MdsMessage {
  role: string;
  content: string;
}

declare module '*.mds' {
  /** Compiled output: a Markdown string (kind='markdown') or an array of chat messages (kind='messages'). */
  const content: string | MdsMessage[];
  export default content;
  /**
   * Compiler metadata: non-fatal warnings and transitive file dependencies.
   * `dependencies` entries are project-root-relative POSIX paths — never
   * absolute host paths, because this literal is embedded in production bundles.
   */
  export const metadata: { warnings: string[]; dependencies: string[] };
}
