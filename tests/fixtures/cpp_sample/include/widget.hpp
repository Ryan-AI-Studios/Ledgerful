#pragma once

// Shared sample header for C++ native index fixture (0165).

class Widget {
public:
    Widget();
    ~Widget();
    int value() const;
    Widget& operator+=(int n);
    explicit operator bool() const;

private:
    int x_;
};

struct Point {
    int x;
    int y;
};

int add(int a, int b);
