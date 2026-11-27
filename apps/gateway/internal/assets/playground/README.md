# Scalar API Reference — vendored

`standalone.js` is the self-contained browser build of
[`@scalar/api-reference`](https://www.npmjs.com/package/@scalar/api-reference),
vendored so `/docs` renders with **no request to a third party**. That is a
requirement, not a preference: the playground is served from the cluster to
operators, and a CDN dependency would make an internal tool depend on an external
service being reachable and trustworthy.

| | |
| --- | --- |
| Package | `@scalar/api-reference` |
| Version | `1.85.0` |
| File | `dist/browser/standalone.js` |
| Licence | MIT |
| Retrieved from | `https://registry.npmjs.org/@scalar/api-reference/-/api-reference-1.85.0.tgz` |
| Tarball SHA-256 | `19eb333ee613bd7ab144accf4025e6b457998e607153a1585d5cf85d78bfe952` |
| Size | 4.5 MB, 1.35 MB gzipped |

Unmodified. `internal/playground` compresses it once at startup and serves it with
`Content-Encoding: gzip` when the client accepts gzip, and as-is otherwise. There is
no build step and no bundler: the file is served byte for byte as it shipped.

## Updating

```sh
version=1.86.0                       # check the npm registry first
curl -sSL -o pkg.tgz "https://registry.npmjs.org/@scalar/api-reference/-/api-reference-${version}.tgz"
sha256sum pkg.tgz                    # record it in this file
tar xzf pkg.tgz package/dist/browser/standalone.js
cp package/dist/browser/standalone.js internal/assets/playground/standalone.js
```

Then update the table above and run `make verify-full` — the playground test
fetches the page and every asset it references and fails if any resolves outside
the gateway's own origin.

## Licence

```
MIT License

Copyright (c) 2023-present Scalar

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```
