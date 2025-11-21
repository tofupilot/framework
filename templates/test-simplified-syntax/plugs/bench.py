class BenchManager:
    def __init__(self, context, config):
        self.context = context
        self.config = config

    def __enter__(self):
        return self

    def __exit__(self, *args):
        pass
