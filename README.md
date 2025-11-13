# TofuPilot Framework

**Build and deploy hardware tests faster.**

Rust-based test execution engine for hardware manufacturing. Test multiple units in parallel, share instruments safely, and run Python test code.

This framework powers:
- **[TofuPilot Studio](https://tofupilot.com/docs/studio)** - Desktop app for visual test editing and interactive execution
- **[TofuPilot Station](https://tofupilot.com/docs/station)** - Headless runner for production line automation

## Documentation

- **Framework Guide**: [tofupilot.com/docs/framework](https://tofupilot.com/docs/framework)
- **Getting Started**: [tofupilot.com/docs/framework/runner](https://tofupilot.com/docs/framework/runner)
- **Architecture**: [tofupilot.com/docs/framework/architecture](https://tofupilot.com/docs/framework/architecture)

## Core Components

- **Execution Engine**: Parallel test orchestration with resource-aware scheduling
- **Plug System**: Hardware resource lifecycle management
- **Schema**: YAML procedure definitions and validation
- **Python Runtime**: Test execution with environment isolation
- **Measurements**: Test measurement evaluation and limit checking
- **Validation**: Procedure validation and diagnostics

## Key Features

- Parallel test execution with worker pools
- Resource-aware scheduling prevents conflicts
- Automatic retry logic for transient failures
- Real-time progress tracking
- Python test integration
- Comprehensive validation

## Contributing

Contributions welcome! Open an issue or submit a pull request.

## License

MIT License - see [LICENSE](LICENSE) file.

## Links

- [tofupilot.com](https://tofupilot.com)
- [Documentation](https://tofupilot.com/docs)
