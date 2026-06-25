/** A single structured message from a messages-mode MDS compile result. */
interface MdsMessage {
  role: string;
  content: string;
}

declare module '*.mds' {
  /** Compiled output: a Markdown string (kind='markdown') or an array of chat messages (kind='messages'). */
  const content: string | MdsMessage[];
  export default content;
  /** Compiler metadata: non-fatal warnings and transitive file dependencies. */
  export const metadata: { warnings: string[]; dependencies: string[] };
}
