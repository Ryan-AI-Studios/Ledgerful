import os
from pkg.module import thing

def run():
    return os.getenv("DATABASE_URL")
