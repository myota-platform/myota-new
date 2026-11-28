// The build is a static tree served by nginx. Two settings matter:
//
//   base: '/'        — the app is served from the host root, and the API is on the same
//                      origin under /v1, so every call is a relative path and the bundle
//                      contains no other host.
//   build.target     — 'es2022' rather than the default, so the output does not carry
//                      transpiled padding for browsers this console will never meet.
import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'

export default defineConfig({
  plugins: [vue()],
  base: '/',
  build: {
    target: 'es2022',
    outDir: 'dist',
    emptyOutDir: true,
    // No source maps in the image: a map is a copy of the source with the comments,
    // and the source is already public in the repository.
    sourcemap: false,
  },
  test: {
    environment: 'jsdom',
    include: ['src/**/*.test.js'],
  },
})
