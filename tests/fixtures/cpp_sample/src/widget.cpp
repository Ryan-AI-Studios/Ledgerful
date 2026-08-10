#include "widget.hpp"
#include <vector>
#include <cstdio>

// Free function (DoD-2 non-anonymous).
int add(int a, int b) {
    return a + b;
}

Widget::Widget() : x_(0) {}

Widget::~Widget() = default;

int Widget::value() const {
    return x_;
}

Widget& Widget::operator+=(int n) {
    x_ += n;
    return *this;
}

Widget::operator bool() const {
    return x_ != 0;
}

// Same-file call + member unwrap (DoD-3).
int use_widget(Widget* w) {
    int s = add(1, 2);
    if (w) {
        s += w->value();
    }
    return s;
}

// Template call unwrap (DoD-3 / D5).
template <typename T>
T identity(T x) {
    return x;
}

int use_template() {
    return identity<int>(7);
}

// Branched control flow + lambda for complexity (DoD-4).
int branched_score(int n) {
    int total = 0;
    if (n > 0) {
        total += n;
    }
    for (int i = 0; i < n; ++i) {
        if (i % 2 == 0) {
            total += i;
        } else {
            total -= i;
        }
    }
    switch (n) {
    case 0:
        total += 1;
        break;
    case 1:
        total += 2;
        break;
    default:
        total += 3;
        break;
    }
    auto fn = [&](int x) {
        if (x > 0) {
            return x * 2;
        }
        return x;
    };
    total += fn(n);
    try {
        if (n < 0) {
            throw 1;
        }
    } catch (...) {
        total = -1;
    }
    printf("score=%d\n", total);
    return total;
}
