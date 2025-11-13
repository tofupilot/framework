import random


class TestInstrument:
    def __init__(self):
        self.state = {
            "connected": False,
            "id": "",
            "value": 0.0,
        }

    def __del__(self):
        if self.state.get("connected", False):
            print(f"Cleaning up {self.state['id']}")

    def measure(self):
        if not self.state["connected"]:
            self.state["connected"] = True
            self.state["id"] = f"INST-{random.randint(1000, 9999)}"

        self.state["value"] = round(5.0 + random.uniform(-0.5, 0.5), 3)
        return self.state
