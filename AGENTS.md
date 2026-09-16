# Repository Guidelines

## Project structure and module organization

`timeliner-tui` is at its initial setup stage. No application source, tests, assets, dependency manifests, or build configuration are checked in yet.

When adding the initial implementation, document its layout in `README.md`. Group source code by responsibility, keep test fixtures separate from application data, and follow the selected language's directory conventions. Paths such as `src/`, `tests/`, and `assets/` are suggestions, not existing directories.

## Build, test, and development commands

No build, test, or local run commands are configured. When introducing tooling, document exact setup, run, build, format, lint, and test commands in `README.md`, including required tool versions.

Available repository checks:

- `git status --short`: inspect pending changes and untracked files.
- `git diff --check`: check tracked changes for whitespace errors.
- `git diff --cached`: review staged content before committing.

## Coding style and naming conventions

No language-specific indentation rules, formatter, or linter are established. Select conventional tooling alongside the first source files and commit its configuration. Let the formatter define indentation; follow the language's standard naming conventions.

Use descriptive names, keep modules focused, and avoid unrelated formatting changes. Write Markdown with descriptive headings and fenced code blocks for command examples.

## Testing guidelines

No test framework or coverage threshold is configured. Introduce a framework appropriate to the implementation language and document how to run the full suite and individual tests. Name tests after the behavior they verify, following the framework's discovery conventions. Add regression tests for bug fixes and use deterministic fixtures.

## Commit and pull request guidelines

After completing a request, run the applicable validation and commit the changes
automatically. Do not wait for a separate commit request or ask for additional
commit confirmation. Keep commits focused and exclude unrelated pending changes.

The repository has no commit history, so no established message convention can be inferred. Use short, imperative subjects, such as `Add initial application scaffold`, and keep each commit focused.

Pull requests should explain the change, link relevant issues, and report validation commands and results. If checks cannot run, explain why. Include a terminal capture or screenshot for visible interface changes.
