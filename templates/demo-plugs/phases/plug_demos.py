"""Plug demonstration phases"""

import sys
import os



def demo_plug_measurement(phase, run):
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

    


def verify_plug_state(phase, run):
    """Verifies plug state persisted from previous phase"""
    plugs = getattr(run, 'plugs', {})

    if 'Multimeter' in plugs:
        dmm = plugs['Multimeter']
        result = dmm.measure_voltage()
        print(f"✅ DMM measurement: {result}", file=sys.stderr)

        dmm_id = result.get('id') if isinstance(result, dict) else None
        if dmm_id:
            print(f"✅ Plug persisted - DMM ID: {dmm_id}", file=sys.stderr)
            print("🎉 Plug state persistence verified!", file=sys.stderr)
            
        else:
            print("❌ Plug lost connection - no ID found", file=sys.stderr)
            phase.fail("Phase failed")

    if 'Power Supply' in plugs:
        ps = plugs['Power Supply']
        voltage = ps.get_voltage()
        print(f"✅ Power Supply voltage: {voltage}V", file=sys.stderr)

    
