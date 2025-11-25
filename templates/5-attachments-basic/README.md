# Attachments Basic

![5-attachments-basic](cover.png)
Attach files and data to test reports.

## What You'll Learn

- Attach binary data with `attach.data()`
- Attach existing files with `attach.file()`
- Generate data (JSON, logs) during test
- Files are stored with test results

## When to Use This

Use attachments to include supporting data with test results - waveforms, images, logs, calibration data, etc.

## Structure

```
attachments-basic/
├── procedure.yaml          # Procedure definition
├── phases/
│   ├── collect_data.py    # Attaches JSON data
│   └── capture_logs.py    # Attaches log file
└── README.md
```

## Key Concepts

- **attach.data()**: Attach binary data directly (no temp file needed)
- **attach.file()**: Attach existing file from filesystem
- **Formats**: JSON, CSV, images, logs - any file type
- **Storage**: Automatically stored with test results

## Next Steps

- [attachments](../../docs/framework/attachments) - More attachment patterns
- [measurements-basic](../measurements-basic) - Combine with measurements
