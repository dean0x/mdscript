declare module '*.mds' {
  const content: string;
  export default content;
  /** Compiler metadata: non-fatal warnings and transitive file dependencies. */
  export const metadata: { warnings: string[]; dependencies: string[] };
}
