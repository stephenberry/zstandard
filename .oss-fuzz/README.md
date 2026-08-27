# OSS-Fuzz integration template

This directory contains the files needed to onboard `zstandard` to
[Google OSS-Fuzz](https://github.com/google/oss-fuzz). The integration is not
live yet — these files exist so that submitting a PR to `google/oss-fuzz` is
mostly a copy-paste.

## Submission steps

1. Read the OSS-Fuzz [new project
   guide](https://google.github.io/oss-fuzz/getting-started/new-project-guide/).
2. Fork `google/oss-fuzz`.
3. Copy this directory to `projects/zstandard/` in that fork:
   ```
   cp .oss-fuzz/{project.yaml,Dockerfile,build.sh} \
      <oss-fuzz-fork>/projects/zstandard/
   ```
4. Open a PR against `google/oss-fuzz` titled "Add zstandard project."

## Local sanity check

The build script can be exercised with the OSS-Fuzz base builder image:

```sh
docker pull gcr.io/oss-fuzz-base/base-builder-rust
docker build -t zstandard-fuzz .oss-fuzz/
docker run --rm -v $PWD/out:/out zstandard-fuzz \
    bash -c "compile && ls -l /out"
```

## Notes

- The four targets (`frame_parse`, `literals_parse`, `sequence_parse`,
  `full_decode`) are the same ones run by `.github/workflows/fuzz.yml`.
- `address` is the only sanitizer enabled; `memory` is unsupported on Rust
  targets and `undefined` adds little signal beyond what the standard library
  panics already cover.
- `file_github_issue: false` keeps OSS-Fuzz from creating issues directly on
  this repo. Crash reports go to the maintainer's email instead — see
  `SECURITY.md` for the disclosure process before any public discussion.
