# TofuPilot Framework

Open-source test execution framework for hardware testing.

## Overview

This framework provides the core execution engine for running hardware test procedures with:

- **Test Orchestration**: Parallel test execution with resource management
- **Plug System**: Hardware resource lifecycle management and allocation
- **Python Integration**: Execute Python test scripts with proper environment isolation
- **Schema Validation**: YAML-based procedure definitions with validation
- **Measurements**: Evaluation of test measurements against limits

## Architecture

### Core Components

- **Execution Engine** (`execution/`): Orchestrator, worker pool, and job scheduling
- **Plug Manager** (`plugs/`): Resource management and plug lifecycle
- **Schema** (`schema/`): YAML procedure definitions and validation
- **Python Runtime** (`python/`): Python environment resolution and execution
- **Measurements** (`measurements/`): Measurement evaluation and limit checking
- **Validation** (`validation/`): Procedure validation and diagnostics

### Key Features

- Parallel test execution with configurable worker pools
- Resource-aware scheduling to prevent conflicts
- Automatic retry logic for transient failures
- Real-time test progress tracking
- Comprehensive logging and diagnostics

## Usage

This framework is designed to be embedded in applications that need hardware test execution capabilities. It provides:

1. **Procedure Loading**: Parse YAML test procedures
2. **Validation**: Validate procedures before execution
3. **Orchestration**: Execute tests with resource management
4. **Results**: Collect and evaluate test results

## Development

This code is automatically synced from the [TofuPilot monorepo](https://github.com/tofupilot/tofupilot).

Changes should be made in the monorepo at `apps/studio/src-tauri/src/` and will be automatically synced to this repository.

## License

[License TBD]
