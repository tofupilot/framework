# Measurements Basic

Capture and validate with pass/fail criteria.

## What You'll Learn

- Define measurements with name and unit
- Set validators with operators (>=, <=)
- Record measurement values from Python
- Automatic pass/fail based on validators

## When to Use This

Use when you need to measure values and validate them against limits. This is the most common testing pattern in TofuPilot.

## Structure

```
measurements-basic/
├── procedure.yaml              # Defines measurements and validators
├── phases/
│   ├── measure_voltage.py     # Records voltage measurement
│   └── measure_temperature.py # Records temperature measurement
└── README.md
```

## Key Concepts

- **Measurements**: Named values with units (V, °C, A, etc.)
- **Validators**: Define expected ranges with operators
- **Outcome**: Phase passes if all validators pass, fails otherwise

## Next Steps

- [measurements/numeric](../../docs/framework/measurements/numeric) - More validator operators
- [measurements/aggregations](../../docs/framework/measurements/aggregations) - Statistics on arrays
- [operator-ui-basic](../operator-ui-basic) - Add operator interaction
