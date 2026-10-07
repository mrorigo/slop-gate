class Worker:
    async def run(self, item):
        # A comment-only line is not source mass.
        if item and item.ready:
            return "first"
        elif item is None:
            return 'second'
        else:
            def nested(value):
                return value + 1

            mapper = lambda value: value * 2
            return nested(mapper(item))


def test_plain(value):
    return value


@decorate
def decorated(value):
    return value


def match_value(value):
    match value:
        case 0:
            return "zero"
        case _:
            return "other"


def comprehension(values):
    return [value for value in values if value]


@overload
def parse_value(value: str) -> str: ...


@overload
def parse_value(value: int) -> int: ...


def parse_value(value):
    return value


class NumberBox:
    @property
    def value(self):
        return self._value

    @value.setter
    def value(self, value):
        self._value = value
