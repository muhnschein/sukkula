import QtQuick 2.6
import Sailfish.Silica 1.0

// Silica's page stack, for real: pages are created, given `pageStack`,
// walked through Activating/Active and Deactivating/Inactive, and
// destroyed when popped -- so page status handlers, dialogs and the
// window's navigation run on the host. No animation: `busy` is only true
// while a call is in progress, and a test can set `holdBusy` to see what
// the app does while a transition would be running.
Item {
    id: stack

    property var pages: []
    property int depth: 0
    property Item currentPage: null
    property bool holdBusy: false
    readonly property bool busy: stack.holdBusy || stack._working
    property bool _working: false
    // What happened, for tests: "push:ConsentDialog", "pop:SendPage", ...
    property var log: []

    function _name(item) {
        return item && item.objectName ? item.objectName : "page"
    }

    function _component(page) {
        if (page && typeof page.createObject === "function") {
            return page
        }
        var comp = Qt.createComponent(page)
        if (comp.status === Component.Error) {
            console.warn("PageStack stub: " + comp.errorString())
            return null
        }
        return comp
    }

    function _setStatus(item, status) {
        if (item) {
            item.status = status
        }
    }

    function _show(item) {
        stack.currentPage = item
        stack.depth = stack.pages.length
        if (item) {
            item.visible = true
            stack._setStatus(item, PageStatus.Activating)
            stack._setStatus(item, PageStatus.Active)
        }
    }

    function _hide(item) {
        stack._setStatus(item, PageStatus.Deactivating)
        stack._setStatus(item, PageStatus.Inactive)
        if (item) {
            item.visible = false
        }
    }

    function _create(page, properties) {
        var comp = stack._component(page)
        if (!comp) {
            return null
        }
        var props = properties ? properties : {}
        var item = comp.createObject(stack, props)
        if (!item) {
            console.warn("PageStack stub: could not create " + page + ": " + comp.errorString())
            return null
        }
        item.width = Qt.binding(function () { return stack.width })
        item.height = Qt.binding(function () { return stack.height })
        item.pageStack = stack
        return item
    }

    function push(page, properties, operation) {
        stack._working = true
        var item = stack._create(page, properties)
        if (item) {
            stack._hide(stack.currentPage)
            var next = stack.pages.slice(0)
            next.push(item)
            stack.pages = next
            stack.log.push("push:" + stack._name(item))
            stack._show(item)
        }
        stack._working = false
        return item
    }

    function _dropTop() {
        var next = stack.pages.slice(0)
        var top = next.pop()
        stack.pages = next
        stack._hide(top)
        stack.log.push("pop:" + stack._name(top))
        if (top) {
            top.destroy()
        }
        return top
    }

    // Pops the top page, or everything above `target`.
    function pop(target, operation) {
        if (stack.pages.length === 0) {
            return null
        }
        stack._working = true
        var popped = null
        if (target) {
            while (stack.pages.length > 1 && stack.pages[stack.pages.length - 1] !== target) {
                popped = stack._dropTop()
            }
        } else if (stack.pages.length > 1) {
            popped = stack._dropTop()
        }
        stack._show(stack.pages.length > 0 ? stack.pages[stack.pages.length - 1] : null)
        stack._working = false
        return popped
    }

    function replace(page, properties, operation) {
        stack._working = true
        if (stack.pages.length > 0) {
            stack._dropTop()
        }
        stack._working = false
        return stack.push(page, properties, operation)
    }

    // Replaces every page above `target` (all of them when null).
    function replaceAbove(target, page, properties, operation) {
        stack._working = true
        while (stack.pages.length > 0 && stack.pages[stack.pages.length - 1] !== target) {
            stack._dropTop()
        }
        stack._working = false
        return stack.push(page, properties, operation)
    }

    // Takes one page out wherever it is: what accept() and reject() do to
    // a dialog in the real stack.
    function _remove(item) {
        var at = stack.pages.indexOf(item)
        if (at < 0) {
            return
        }
        if (at === stack.pages.length - 1) {
            stack._working = true
            stack._dropTop()
            stack._show(stack.pages.length > 0 ? stack.pages[stack.pages.length - 1] : null)
            stack._working = false
            return
        }
        var next = stack.pages.slice(0)
        next.splice(at, 1)
        stack.pages = next
        stack.depth = next.length
        stack._hide(item)
        stack.log.push("pop:" + stack._name(item))
        item.destroy()
    }

    function find(test) {
        for (var i = stack.pages.length - 1; i >= 0; i--) {
            if (test(stack.pages[i])) {
                return stack.pages[i]
            }
        }
        return null
    }

    function previousPage(page) {
        var at = stack.pages.indexOf(page ? page : stack.currentPage)
        return at > 0 ? stack.pages[at - 1] : null
    }

    function clear() {
        while (stack.pages.length > 0) {
            stack._dropTop()
        }
        stack._show(null)
    }

    function completeAnimation() {}
}
