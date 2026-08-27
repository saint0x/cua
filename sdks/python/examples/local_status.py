from cua_sdk import Cua


cua = Cua.connect(profile="default")
print(cua.status())
