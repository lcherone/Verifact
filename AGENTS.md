# Verifact repository guidance

- Keep product code, documentation and test fixtures independent of projects used to test Verifact.
- Do not add their names, local checkout paths, site URLs, private suites or screenshots to this repository.
- Detect test frameworks through generic runner/configuration evidence, never customer-specific package names.
- Keep configured projects in Verifact's application-data directory. Store real-project acceptance artifacts outside this checkout; runtime Chromium logs belong under the application-data `logs/` directory.
- Use synthetic, generically named fixtures for automated coverage.
- Follow README.md for build/run commands and TESTING.md for verification. Preserve unrelated changes and report the checks actually executed.
