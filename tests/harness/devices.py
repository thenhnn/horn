from abc import ABC, abstractmethod


class Device(ABC):
    @abstractmethod
    def qemu_arguments(self):
        pass
