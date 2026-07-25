package pkg

import (
	"errors"
	"fmt"
	"log/slog"
	"net/http"

	"github.com/gin-gonic/gin"
)

var ErrNotFound = errors.New("not found")

func localHelper(u *User) string {
	return u.DisplayName()
}

func GetUser(w http.ResponseWriter, r *http.Request) {
	u := DefaultUser
	name := localHelper(&u)
	slog.Info("get user", "name", name)
	if err := validate(); err != nil {
		if errors.Is(err, ErrNotFound) {
			slog.Error("not found")
			return
		}
		_ = fmt.Errorf("wrap: %w", err)
	}
	// Cross-package unresolved call (fmt import alias).
	fmt.Println(name)
	_, _ = w.Write([]byte(name))
}

func validate() error {
	return ErrNotFound
}

func CreateUser(c *gin.Context) {}

func RegisterRoutes() {
	http.HandleFunc("GET /users/{id}", GetUser)
	http.HandleFunc("/health", GetUser)

	r := gin.Default()
	r.GET("/users", listUsers)
	r.POST("/users", CreateUser)

	var api = gin.New()
	api.GET("/items", listUsers)
}

func listUsers(c *gin.Context) {}
