from shared.events import ExampleGreeting


def test_payload() -> None:
    assert ExampleGreeting(name="ada").model_dump(mode="json") == {"name": "ada"}
