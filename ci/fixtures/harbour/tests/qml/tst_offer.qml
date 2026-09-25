// Outside qml/, so never installed: the check must not read it. QtTest and
// an absolute import are both things Harbour would reject in the package.
import QtTest 1.0
import "/usr/share/harbour-sukkula/qml/components"

TestCase {
    name: "Offer"
}
