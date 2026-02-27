# TofuPilot Framework

[![Downloads](https://img.shields.io/badge/dynamic/json?url=https%3A%2F%2Fapi.crabnebula.app%2Fdirectory%2Frspc%2Finsights.application.get%3Finput%3D%257B%2522appId%2522%253A%252201K81002F5F23QKG30T6HXBR41%2522%252C%2522from%2522%253Anull%252C%2522to%2522%253Anull%257D&query=%24.result.data.downloads.total&label=downloads&color=blue)](https://web.crabnebula.cloud/tofupilot/tofupilot-studio/releases)

**TofuPilot Framework** is an open-source test orchestration framework for hardware manufacturing.

## Features

**TofuPilot Framework** comes battery-included with many features useful for hardware testing development.

<img src="https://raw.githubusercontent.com/tofupilot/framework/main/.github/images/readme-features.png" alt="TofuPilot Framework Features" width="200">

- **Phases:** Break tests into phases with dependencies
- **Operator UI:** Declare components in YAML, no frontend code
- **Parallel execution:** Run multiple phases simultaneously
- **Measurements:** Define limits, automatic pass/fail
- **Plugs:** Equipment drivers as Python classes
- **Cross-platform:** Windows, Linux, macOS
- **Dashboard integration:** Integrates with [**TofuPilot Dashboard**](https://tofupilot.com/docs/dashboard) for storage and analytics, or run standalone

## Get Started

The best way to get started with **TofuPilot Framework** is to use **TofuPilot Studio**, a cross-platform desktop app for developing and debugging your test procedures.

<table>
<tr>
<td width="33%">

**Home**

</td>
<td width="33%">

**Edit**

</td>
<td width="33%">

**Run**

</td>
</tr>
<tr>
<td>

<img src="https://raw.githubusercontent.com/tofupilot/framework/main/.github/images/studio-home.png" alt="TofuPilot Studio Home" width="300">

Browse projects and templates

</td>
<td>

<img src="https://raw.githubusercontent.com/tofupilot/framework/main/.github/images/studio-edit.png" alt="TofuPilot Studio Editor" width="300">

Edit your procedure YAML visually

</td>
<td>

<img src="https://raw.githubusercontent.com/tofupilot/framework/main/.github/images/studio-run.png" alt="TofuPilot Studio Run" width="300">

Run and debug your procedure

</td>
</tr>
</table>

1. Download the latest version for:
   - [Windows](https://cdn.crabnebula.app/download/tofupilot/tofupilot-studio/latest/platform/nsis-x86_64)
   - [Linux](https://cdn.crabnebula.app/download/tofupilot/tofupilot-studio/latest/platform/appimage-x86_64)
   - [macOS](https://cdn.crabnebula.app/download/tofupilot/tofupilot-studio/latest/platform/dmg-aarch64)
2. Clone a template from the home page
3. Run it
4. Customize and extend your test procedure

## Deploy

Once you've developed your test procedure in **TofuPilot Studio**, you can deploy it to production test stations with **TofuPilot Station** (coming soon).

Station is a production-optimized application with simplified operator interface, automatic report sync to Dashboard, auto-updates from Git, and production hardening for factory floor deployment.

## Templates

You can clone these templates from [**TofuPilot Studio**](https://www.tofupilot.com) to get started.

<table>
<tr>
<td width="33%">

**[Hello World](../procedures/templates/1-hello-world)**

</td>
<td width="33%">

**[Measurements](../procedures/templates/2-measurements-basic)**

</td>
<td width="33%">

**[Operator UI](../procedures/templates/3-operator-ui-basic)**

</td>
</tr>
<tr>
<td>

[<img src="https://raw.githubusercontent.com/tofupilot/framework/main/templates/1-hello-world/cover.png" width="200">](https://github.com/tofupilot/framework/tree/main/templates/1-hello-world)

Simple procedure showing basic structure

</td>
<td>

[<img src="https://raw.githubusercontent.com/tofupilot/framework/main/templates/2-measurements-basic/cover.png" width="200">](https://github.com/tofupilot/framework/tree/main/templates/2-measurements-basic)

Capture data with pass/fail criteria

</td>
<td>

[<img src="https://raw.githubusercontent.com/tofupilot/framework/main/templates/3-operator-ui-basic/cover.png" width="200">](https://github.com/tofupilot/framework/tree/main/templates/3-operator-ui-basic)

Interactive interfaces with input and display

</td>
</tr>
<tr>
<td width="33%">

**[Plugs](https://github.com/tofupilot/framework/tree/main/templates/4-plugs-basic)**

</td>
<td width="33%">

**[Attachments](https://github.com/tofupilot/framework/tree/main/templates/5-attachments-basic)**

</td>
<td width="33%">

**[Parallel Phases](https://github.com/tofupilot/framework/tree/main/templates/6-phases-parallel)**

</td>
</tr>
<tr>
<td>

[<img src="https://raw.githubusercontent.com/tofupilot/framework/main/templates/4-plugs-basic/cover.png" width="200">](https://github.com/tofupilot/framework/tree/main/templates/4-plugs-basic)

Persistent resources like test instruments

</td>
<td>

[<img src="https://raw.githubusercontent.com/tofupilot/framework/main/templates/5-attachments-basic/cover.png" width="200">](https://github.com/tofupilot/framework/tree/main/templates/5-attachments-basic)

Attach files and data to test reports

</td>
<td>

[<img src="https://raw.githubusercontent.com/tofupilot/framework/main/templates/6-phases-parallel/cover.png" width="200">](https://github.com/tofupilot/framework/tree/main/templates/6-phases-parallel)

Run independent test phases simultaneously

</td>
</tr>
</table>

## Documentation

You can learn more about all TofuPilot features in the [docs](https://tofupilot.com/docs/framework).

[<img src="https://raw.githubusercontent.com/tofupilot/framework/main/.github/images/readme-docs.png" alt="Documentation" width="200">](https://tofupilot.com/docs/framework)

You can raise an issue on [GitHub](https://github.com/tofupilot/framework/issues) or [Discord](https://discord.gg/fK3AeTyngh) for doc improvements.

## Community

You can join our [Discord](https://discord.gg/fK3AeTyngh) server to ask anything, report an issue, or get latest updates on TofuPilot features and changes.

[<img src="https://raw.githubusercontent.com/tofupilot/framework/main/.github/images/readme-discord.png" alt="Join our Discord" width="200">](https://discord.gg/fK3AeTyngh)

You can also raise issues on this [repository](https://github.com/tofupilot/framework/issues) directly.

## About

**TofuPilot Framework** is maintained by the crew of [TofuPilot](https://www.tofupilot.com/about). 

<img src="https://raw.githubusercontent.com/tofupilot/framework/main/.github/images/readme-about.png" alt="About TofuPilot" width="200">

We're a team of robotics, data and quality engineers based out of Switzerland who believe hardware tests deserve the same love as your production code.

## License

TofuPilot Framework is open-source under the [MIT license](LICENSE), meaning you can use it freely for any purpose—commercial or personal.

<img src="https://raw.githubusercontent.com/tofupilot/framework/main/.github/images/readme-license.png" alt="License" width="200">

TofuPilot Studio and Dashboard source code are not yet open-source, though we're exploring this for the future.

## Support Us

We'd love your support through feedback, bug reports, feature requests, and spreading the word to your hardware friends.

[<img src="https://raw.githubusercontent.com/tofupilot/framework/main/.github/images/readme-support.png" alt="Support Us" width="200">](https://www.tofupilot.com/pricing)

The best way to support our team is getting a [TofuPilot Pro](https://www.tofupilot.com/pricing) account to get plug-and-play database and analytics for all your tests.
