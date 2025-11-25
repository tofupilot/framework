# Operator UI Basic

Interactive interfaces with input and display.

## What You'll Learn

- Numeric input for entering values
- Slider for selecting range values
- Text display updated from Python
- Progress bar showing test progress

## When to Use This

Use when you need operator interaction during testing - entering test parameters, displaying live status, showing progress, etc.

## Structure

```
operator-ui-basic/
├── procedure.yaml          # Defines UI components
├── phases/
│   ├── set_parameters.py  # Phase with input components
│   └── run_test.py        # Phase with display updates
└── README.md
```

## Key Concepts

- **Input Components**: number_input, slider for operator data entry
- **Display Components**: text, progress bar for live updates
- **Reading Inputs**: Access operator values with `ui.component_key`
- **Updating Displays**: Change display values from Python using `ui.component_key = value`

## Next Steps

- [operator-ui/number-input](../../docs/framework/operator-ui/number-input) - More input types
- [operator-ui/slider](../../docs/framework/operator-ui/slider) - Range inputs
- [measurements-basic](../measurements-basic) - Combine with measurements
