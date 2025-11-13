# TofuPilot Framework

**Build and deploy hardware tests faster.**

The **TofuPilot Framework** is an open-source test execution framework for hardware testing. It provides orchestration, resource management, and execution capabilities for YAML-defined hardware test procedures.

## 📚 Documentation

For complete documentation, visit:
- **Framework Guide**: [tofupilot.com/docs/framework](https://tofupilot.com/docs/framework)
- **Getting Started**: [tofupilot.com/docs/framework/runner](https://tofupilot.com/docs/framework/runner)
- **Architecture**: [tofupilot.com/docs/framework/architecture](https://tofupilot.com/docs/framework/architecture)

## 🏗️ Architecture

### Core Components

- **Execution Engine** (`execution/`): Parallel test orchestration with resource-aware scheduling
- **Plug System** (`plugs/`): Hardware resource lifecycle management and allocation
- **Schema** (`schema/`): YAML procedure definitions and validation
- **Python Runtime** (`python/`): Python environment resolution and test execution
- **Measurements** (`measurements/`): Test measurement evaluation and limit checking
- **Validation** (`validation/`): Procedure validation and diagnostic reporting

## 🚀 Products

The **TofuPilot Framework** powers multiple products:

### TofuPilot Studio

**TofuPilot Studio** is a desktop application that embeds this framework to provide:
- Visual procedure editor
- Interactive test execution with real-time progress
- Result visualization and debugging
- Procedure validation and diagnostics

Download: [tofupilot.com/docs/studio](https://tofupilot.com/docs/studio)

### TofuPilot Station

**TofuPilot Station** is a headless runner that uses this framework for:
- Production line test execution
- Automated result reporting to TofuPilot Dashboard
- Hardware resource management
- Continuous operation in manufacturing environments

Deploy: [tofupilot.com/docs/station](https://tofupilot.com/docs/station)

### TofuPilot Dashboard

**TofuPilot Dashboard** is the cloud platform for:
- Test result analytics and visualization
- Production monitoring and alerting
- Historical data analysis
- Team collaboration

Learn more: [tofupilot.app](https://tofupilot.app)

## 🛠️ Key Features

- ⚡ **Parallel Execution**: Run multiple tests concurrently with worker pools
- 🎯 **Resource Management**: Prevent conflicts with resource-aware scheduling
- 🔄 **Auto Retry**: Configurable retry logic for transient failures
- 📊 **Real-time Progress**: Track test execution with detailed status updates
- 🐍 **Python Integration**: Execute Python test code with environment isolation
- ✅ **Validation**: Comprehensive procedure validation before execution

## 🤝 Contributing

Contributions are welcome! Please open an issue or submit a pull request.

## 📝 License

MIT License - see [LICENSE](LICENSE) file for details.

## 🔗 Links

- **Website**: [tofupilot.com](https://tofupilot.com)
- **Documentation**: [tofupilot.com/docs](https://tofupilot.com/docs)
- **Dashboard**: [tofupilot.app](https://tofupilot.app)
