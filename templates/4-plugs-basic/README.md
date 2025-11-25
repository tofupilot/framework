# Plugs Basic

Persistent resources like test instruments.

## What You'll Learn

- Define plugs as Python classes
- Plug initialization with `__init__`
- Plug cleanup with `__del__`
- Call plug methods from multiple phases
- Automatic plug lifecycle management

## When to Use This

Use plugs when you need to share resources across phases - test equipment, hardware interfaces, database connections, etc.

## Structure

```
plugs-basic/
├── procedure.yaml          # Defines plug
├── instruments/
│   └── power_supply.py    # Plug implementation
├── phases/
│   ├── set_voltage.py     # Uses plug
│   └── measure_output.py  # Uses same plug
└── README.md
```

## Key Concepts

- **Plugs**: Python classes for persistent resources
- **Lifecycle**: Init before phases, cleanup after phases
- **Sharing**: Same plug instance available to all phases
- **Isolation**: Plugs run in separate process for safety

## Next Steps

- [plugs](../../docs/framework/plugs) - Advanced plug patterns
- [phases-complete](../phases-complete) - Setup/teardown phases
