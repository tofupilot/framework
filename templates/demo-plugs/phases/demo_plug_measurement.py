import sys

def demo_plug_measurement(phase, run, ui):
    """Demonstrates using plugs for measurements"""
    plugs = getattr(run, 'plugs', {})

    if 'Multimeter' in plugs:
        dmm = plugs['Multimeter']
        result = dmm.identify()
        print(f"📊 Multimeter identified: {result}", file=sys.stderr)

    if 'Power Supply' in plugs:
        ps = plugs['Power Supply']
        ps.set_voltage(5.0)
        print(f"⚡ Power supply set to 5.0V", file=sys.stderr)

    
