import time

import pytest


@pytest.mark.parametrize("n", range(12))
def test_fast(n):
    time.sleep(0.05)
    assert n >= 0


def test_broken():
    assert 1 + 1 == 3


@pytest.mark.skip(reason="demo")
def test_skipped():
    pass
