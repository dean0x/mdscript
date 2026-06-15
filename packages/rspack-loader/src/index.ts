import { createMdsLoader } from '@mdscript/bundler-utils';

// NOTE: options are captured from the first call. Rspack loaders are
// stateless functions invoked per-file; options come from the rspack
// config and do not change across loader invocations within a single
// build. Multiple compiler instances with different options are not
// supported by a module-level singleton — use separate rspack processes
// in that scenario.
//
// The createMdsLoader() factory is called once at module scope to preserve
// process-wide singleton semantics. Each `import` of this loader module
// shares one lazy-init instance.
const { loader, _resetForTesting, _setTransformerForTesting } = createMdsLoader();

export default loader;
export { _resetForTesting, _setTransformerForTesting };
