# Disposable VM pressure-guard cleanup regression — 2026-11-09

The focused qualification VM helper suite passed **40/40** with:

```sh
PYTHONPATH=scripts python3 -m unittest scripts.test_dux_qualification_vm
```

Runtime: 0.132 seconds. Output is in `test.log`, SHA-256
`5e64dcb0eb8312d2180972afed36e92f2a5a312aa3fd1c92ee6c6f48de125046`.
The added cases simulate a concurrent cleanup removing the validated run
directory and verify the pressure event is emitted before guest cleanup can
fail. This is helper-level regression coverage, not the real Debian package
lifecycle.
